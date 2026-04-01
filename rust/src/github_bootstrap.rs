use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFieldMode {
    Preserve,
    Normalize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapPlan {
    pub changes: Vec<String>,
    pub already_satisfied: Vec<String>,
}

impl BootstrapPlan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct BootstrapOptions<'a> {
    pub token: &'a str,
    pub owner: &'a str,
    pub repo: &'a str,
    pub project_owner: Option<&'a str>,
    pub project_number: Option<&'a str>,
    pub status_field_name: &'a str,
    pub priority_field_name: &'a str,
    pub status_field_mode: StatusFieldMode,
    pub status_options: Vec<String>,
    pub skip_labels: bool,
    pub skip_priority_field: bool,
}

#[derive(Debug, Clone)]
pub struct RepoLabelSpec {
    pub name: String,
    pub color: &'static str,
    pub description: String,
}

#[derive(Debug, Clone)]
struct StatusOptionSpec {
    name: String,
    project_color: &'static str,
    label_color: &'static str,
    description: String,
}

#[derive(Debug, Deserialize)]
struct GhProjectFieldsEnvelope {
    #[serde(default)]
    fields: Vec<ProjectField>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectField {
    id: Option<String>,
    name: String,
    #[serde(rename = "dataType")]
    data_type: Option<String>,
    #[serde(default)]
    options: Vec<ProjectFieldOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectFieldOption {
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RestLabel {
    name: String,
    color: String,
    description: Option<String>,
}

enum BootstrapAction {
    CreateLabel(RepoLabelSpec),
    UpdateLabel(RepoLabelSpec),
    CreateStatusField,
    UpdateStatusField { field_id: String },
    CreatePriorityField,
}

pub fn derive_status_option_names(
    active_states: &[String],
    terminal_states: &[String],
    claimable_states: &[String],
    in_progress_state: Option<&str>,
    human_review_state: Option<&str>,
    done_state: Option<&str>,
) -> Vec<String> {
    let mut names = Vec::new();

    for name in active_states
        .iter()
        .chain(terminal_states.iter())
        .chain(claimable_states.iter())
    {
        push_unique_name(&mut names, name);
    }

    for value in [in_progress_state, human_review_state, done_state]
        .into_iter()
        .flatten()
    {
        if !is_no_status_change(value) {
            push_unique_name(&mut names, value);
        }
    }

    names
}

pub fn default_label_specs(status_options: &[String]) -> Vec<RepoLabelSpec> {
    let mut specs = fixed_label_specs();
    for status in status_option_specs(status_options) {
        if specs
            .iter()
            .any(|spec| spec.name.eq_ignore_ascii_case(&status.name))
        {
            continue;
        }
        specs.push(RepoLabelSpec {
            name: status.name,
            color: status.label_color,
            description: status.description,
        });
    }
    specs
}

pub fn inspect_bootstrap_plan(options: &BootstrapOptions<'_>) -> Result<BootstrapPlan> {
    let (changes, already_satisfied, _) = build_bootstrap_plan(options)?;
    Ok(BootstrapPlan {
        changes,
        already_satisfied,
    })
}

pub fn apply_bootstrap_plan(options: &BootstrapOptions<'_>) -> Result<BootstrapPlan> {
    let (changes, already_satisfied, actions) = build_bootstrap_plan(options)?;
    for action in actions {
        apply_action(options, action)?;
    }
    Ok(BootstrapPlan {
        changes,
        already_satisfied,
    })
}

pub fn inspect_repo_label_readiness(
    token: &str,
    owner: &str,
    repo: &str,
    desired_specs: &[RepoLabelSpec],
) -> Result<(Vec<String>, Vec<String>)> {
    let existing = list_labels(token, owner, repo)?;
    let mut missing = Vec::new();
    let mut divergent = Vec::new();
    for spec in desired_specs {
        match existing
            .iter()
            .find(|label| label.name.eq_ignore_ascii_case(&spec.name))
        {
            None => missing.push(spec.name.clone()),
            Some(label) => {
                let description = label.description.as_deref().unwrap_or_default();
                if !label.color.eq_ignore_ascii_case(spec.color) || description != spec.description
                {
                    divergent.push(spec.name.clone());
                }
            }
        }
    }
    Ok((missing, divergent))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFieldReadiness {
    pub status_present: bool,
    pub priority_present: bool,
    pub missing_status_options: Vec<String>,
}

pub fn inspect_project_field_readiness(
    token: &str,
    project_owner: &str,
    project_number: &str,
    status_field_name: &str,
    priority_field_name: &str,
    desired_status_options: &[String],
) -> Result<ProjectFieldReadiness> {
    let fields = list_project_fields(token, project_owner, project_number)?;
    let status_field = fields
        .iter()
        .find(|field| field.name.eq_ignore_ascii_case(status_field_name));
    let has_status = status_field.is_some();
    let has_priority = fields.iter().any(|field| {
        field.name.eq_ignore_ascii_case(priority_field_name)
            && field
                .data_type
                .as_deref()
                .map(|value| value.eq_ignore_ascii_case("NUMBER"))
                .unwrap_or(true)
    });
    let existing_names = status_field
        .map(|field| {
            field
                .options
                .iter()
                .map(|option| option.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let missing_status_options = desired_status_options
        .iter()
        .filter(|desired| {
            !existing_names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(desired))
        })
        .cloned()
        .collect::<Vec<_>>();

    Ok(ProjectFieldReadiness {
        status_present: has_status,
        priority_present: has_priority,
        missing_status_options,
    })
}

fn build_bootstrap_plan(
    options: &BootstrapOptions<'_>,
) -> Result<(Vec<String>, Vec<String>, Vec<BootstrapAction>)> {
    let mut changes = Vec::new();
    let mut already_satisfied = Vec::new();
    let mut actions = Vec::new();
    let desired_label_specs = default_label_specs(&options.status_options);

    if !options.skip_labels {
        let existing = list_labels(options.token, options.owner, options.repo)?;
        for spec in desired_label_specs {
            match existing
                .iter()
                .find(|label| label.name.eq_ignore_ascii_case(&spec.name))
            {
                None => {
                    changes.push(format!("create repo label `{}`", spec.name));
                    actions.push(BootstrapAction::CreateLabel(spec));
                }
                Some(label) => {
                    let description = label.description.as_deref().unwrap_or_default();
                    if label.color.eq_ignore_ascii_case(spec.color)
                        && description == spec.description
                    {
                        already_satisfied.push(format!("label `{}` already matches", spec.name));
                    } else {
                        changes.push(format!("update repo label `{}`", spec.name));
                        actions.push(BootstrapAction::UpdateLabel(spec));
                    }
                }
            }
        }
    }

    let Some(project_number) = options
        .project_number
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok((changes, already_satisfied, actions));
    };
    let project_owner = options
        .project_owner
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(options.owner);
    let fields = list_project_fields(options.token, project_owner, project_number)?;
    let status_field = fields
        .iter()
        .find(|field| field.name.eq_ignore_ascii_case(options.status_field_name));

    match status_field {
        None => {
            changes.push(format!(
                "create project field `{}` with selected setup statuses: {}",
                options.status_field_name,
                options.status_options.join(", ")
            ));
            actions.push(BootstrapAction::CreateStatusField);
        }
        Some(field) if options.status_field_mode == StatusFieldMode::Normalize => {
            let current_names = field
                .options
                .iter()
                .map(|option| option.name.clone())
                .collect::<Vec<_>>();
            let desired_names = options.status_options.clone();
            if current_names == desired_names {
                already_satisfied.push(format!(
                    "project field `{}` already matches selected setup statuses",
                    options.status_field_name
                ));
            } else {
                let Some(field_id) = field.id.as_ref() else {
                    return Err(anyhow!(
                        "project field `{}` is missing an id; cannot normalize",
                        options.status_field_name
                    ));
                };
                changes.push(format!(
                    "normalize project field `{}` to selected setup statuses: {}",
                    options.status_field_name,
                    options.status_options.join(", ")
                ));
                actions.push(BootstrapAction::UpdateStatusField {
                    field_id: field_id.clone(),
                });
            }
        }
        Some(_) => {
            already_satisfied.push(format!(
                "project field `{}` exists and preserve mode leaves it unchanged",
                options.status_field_name
            ));
        }
    }

    if !options.skip_priority_field {
        if fields
            .iter()
            .any(|field| field.name.eq_ignore_ascii_case(options.priority_field_name))
        {
            already_satisfied.push(format!(
                "project field `{}` already exists",
                options.priority_field_name
            ));
        } else {
            changes.push(format!(
                "create numeric project field `{}`",
                options.priority_field_name
            ));
            actions.push(BootstrapAction::CreatePriorityField);
        }
    }

    Ok((changes, already_satisfied, actions))
}

fn apply_action(options: &BootstrapOptions<'_>, action: BootstrapAction) -> Result<()> {
    let desired_status_specs = status_option_specs(&options.status_options);
    match action {
        BootstrapAction::CreateLabel(spec) => {
            create_label(options.token, options.owner, options.repo, &spec)
        }
        BootstrapAction::UpdateLabel(spec) => {
            update_label(options.token, options.owner, options.repo, &spec)
        }
        BootstrapAction::CreateStatusField => create_project_field(
            options.token,
            options.project_owner.unwrap_or(options.owner),
            options
                .project_number
                .ok_or_else(|| anyhow!("missing project number for status field creation"))?,
            options.status_field_name,
            &desired_status_specs,
            ProjectFieldType::SingleSelect,
        ),
        BootstrapAction::UpdateStatusField { field_id } => update_status_field(
            options.token,
            &field_id,
            options.status_field_name,
            &desired_status_specs,
        ),
        BootstrapAction::CreatePriorityField => create_project_field(
            options.token,
            options.project_owner.unwrap_or(options.owner),
            options
                .project_number
                .ok_or_else(|| anyhow!("missing project number for priority field creation"))?,
            options.priority_field_name,
            &[],
            ProjectFieldType::Number,
        ),
    }
}

fn list_labels(token: &str, owner: &str, repo: &str) -> Result<Vec<RestLabel>> {
    let mut page = 1;
    let mut labels = Vec::new();
    loop {
        let response = gh_json(
            token,
            &[
                "api".to_string(),
                format!("repos/{owner}/{repo}/labels?per_page=100&page={page}"),
            ],
            None,
        )?;
        let page_labels = serde_json::from_value::<Vec<RestLabel>>(response)
            .context("invalid GitHub labels payload")?;
        if page_labels.is_empty() {
            break;
        }
        labels.extend(page_labels);
        page += 1;
    }
    Ok(labels)
}

fn create_label(token: &str, owner: &str, repo: &str, spec: &RepoLabelSpec) -> Result<()> {
    let _ = gh_json(
        token,
        &[
            "api".to_string(),
            format!("repos/{owner}/{repo}/labels"),
            "--method".to_string(),
            "POST".to_string(),
            "-f".to_string(),
            format!("name={}", spec.name),
            "-f".to_string(),
            format!("color={}", spec.color),
            "-f".to_string(),
            format!("description={}", spec.description),
        ],
        None,
    )?;
    Ok(())
}

fn update_label(token: &str, owner: &str, repo: &str, spec: &RepoLabelSpec) -> Result<()> {
    let encoded_name = url_encode(&spec.name);
    let _ = gh_json(
        token,
        &[
            "api".to_string(),
            format!("repos/{owner}/{repo}/labels/{encoded_name}"),
            "--method".to_string(),
            "PATCH".to_string(),
            "-f".to_string(),
            format!("new_name={}", spec.name),
            "-f".to_string(),
            format!("color={}", spec.color),
            "-f".to_string(),
            format!("description={}", spec.description),
        ],
        None,
    )?;
    Ok(())
}

fn list_project_fields(
    token: &str,
    owner: &str,
    project_number: &str,
) -> Result<Vec<ProjectField>> {
    let response = gh_json(
        token,
        &[
            "project".to_string(),
            "field-list".to_string(),
            project_number.to_string(),
            "--owner".to_string(),
            owner.to_string(),
            "--format".to_string(),
            "json".to_string(),
        ],
        None,
    )?;

    if let Ok(envelope) = serde_json::from_value::<GhProjectFieldsEnvelope>(response.clone()) {
        return Ok(envelope.fields);
    }

    serde_json::from_value::<Vec<ProjectField>>(response)
        .context("invalid GitHub project field-list payload")
}

enum ProjectFieldType {
    SingleSelect,
    Number,
}

fn create_project_field(
    token: &str,
    owner: &str,
    project_number: &str,
    field_name: &str,
    status_specs: &[StatusOptionSpec],
    field_type: ProjectFieldType,
) -> Result<()> {
    let mut args = vec![
        "project".to_string(),
        "field-create".to_string(),
        project_number.to_string(),
        "--owner".to_string(),
        owner.to_string(),
        "--name".to_string(),
        field_name.to_string(),
        "--data-type".to_string(),
        match field_type {
            ProjectFieldType::SingleSelect => "SINGLE_SELECT".to_string(),
            ProjectFieldType::Number => "NUMBER".to_string(),
        },
    ];
    if matches!(field_type, ProjectFieldType::SingleSelect) {
        args.push("--single-select-options".to_string());
        args.push(
            status_specs
                .iter()
                .map(|spec| spec.name.clone())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    gh_command(token, &args, None)?;
    Ok(())
}

fn update_status_field(
    token: &str,
    field_id: &str,
    field_name: &str,
    status_specs: &[StatusOptionSpec],
) -> Result<()> {
    let options = status_specs
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "color": spec.project_color,
                "description": spec.description,
            })
        })
        .collect::<Vec<_>>();
    let payload = json!({
        "query": r#"
            mutation UpdateProjectField($fieldId: ID!, $name: String!, $options: [ProjectV2SingleSelectFieldOptionInput!]) {
              updateProjectV2Field(
                input: {
                  fieldId: $fieldId
                  name: $name
                  singleSelectOptions: $options
                }
              ) {
                projectV2Field {
                  ... on ProjectV2SingleSelectField {
                    id
                    name
                  }
                }
              }
            }
        "#,
        "variables": {
            "fieldId": field_id,
            "name": field_name,
            "options": options,
        },
    });
    let _ = gh_json(
        token,
        &[
            "api".to_string(),
            "graphql".to_string(),
            "--input".to_string(),
            "-".to_string(),
        ],
        Some(payload),
    )?;
    Ok(())
}

fn gh_json(token: &str, args: &[String], payload: Option<JsonValue>) -> Result<JsonValue> {
    let stdout = gh_command(token, args, payload)?;
    if stdout.trim().is_empty() {
        return Ok(JsonValue::Null);
    }
    serde_json::from_str(stdout.trim()).context("failed to decode gh JSON output")
}

fn gh_command(token: &str, args: &[String], payload: Option<JsonValue>) -> Result<String> {
    let mut command = Command::new("gh");
    command
        .args(args)
        .env("GITHUB_TOKEN", token)
        .env("GH_TOKEN", token)
        .stdin(if payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn gh {}", args.join(" ")))?;
    if let Some(payload) = payload {
        let mut stdin = child.stdin.take().context("failed to open gh stdin")?;
        stdin
            .write_all(payload.to_string().as_bytes())
            .context("failed to write gh stdin payload")?;
    }

    let output = child
        .wait_with_output()
        .context("failed to wait for gh command")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(anyhow!(
            "gh command failed (gh {}): {}",
            args.join(" "),
            if stderr.is_empty() { stdout } else { stderr }
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn fixed_label_specs() -> Vec<RepoLabelSpec> {
    vec![
        RepoLabelSpec {
            name: "kairastra".to_string(),
            color: "5319e7",
            description: "Tracked by the Kairastra orchestration workflow.".to_string(),
        },
        RepoLabelSpec {
            name: "agent:codex".to_string(),
            color: "1f6feb",
            description: "Assigned to Codex-driven automation.".to_string(),
        },
        RepoLabelSpec {
            name: "agent:claude".to_string(),
            color: "8250df",
            description: "Assigned to Claude-driven automation.".to_string(),
        },
        RepoLabelSpec {
            name: "agent:gemini".to_string(),
            color: "0e8a16",
            description: "Assigned to Gemini-driven automation.".to_string(),
        },
    ]
}

fn status_option_specs(status_options: &[String]) -> Vec<StatusOptionSpec> {
    status_options
        .iter()
        .map(|name| {
            let normalized = normalize_name(name);
            let (project_color, label_color, description) = match normalized.as_str() {
                "backlog" => (
                    "GRAY",
                    "6e7781",
                    "Out of scope until moved into the active queue.".to_string(),
                ),
                "todo" => (
                    "BLUE",
                    "1f6feb",
                    "Queued for Kairastra to pick up.".to_string(),
                ),
                "in progress" => (
                    "YELLOW",
                    "fbca04",
                    "Actively being worked by Kairastra.".to_string(),
                ),
                "human review" => (
                    "PURPLE",
                    "8250df",
                    "Waiting for human review or approval.".to_string(),
                ),
                "merging" => (
                    "PINK",
                    "d4c5f9",
                    "Approved and ready for landing.".to_string(),
                ),
                "rework" => (
                    "ORANGE",
                    "e99695",
                    "Changes requested and work needs another pass.".to_string(),
                ),
                "done" => ("GREEN", "0e8a16", "Completed and landed.".to_string()),
                "closed" => ("GRAY", "6e7781", "Closed issue state.".to_string()),
                "cancelled" => (
                    "RED",
                    "d73a4a",
                    "Stopped intentionally without completion.".to_string(),
                ),
                "duplicate" => ("GRAY", "cfd3d7", "Superseded by another issue.".to_string()),
                _ if normalized.contains("review") => (
                    "PURPLE",
                    "8250df",
                    format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                ),
                _ if normalized.contains("progress") || normalized.contains("doing") => (
                    "YELLOW",
                    "fbca04",
                    format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                ),
                _ if normalized.contains("merge") => (
                    "PINK",
                    "d4c5f9",
                    format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                ),
                _ if normalized.contains("rework") => (
                    "ORANGE",
                    "e99695",
                    format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                ),
                _ if normalized.contains("done")
                    || normalized.contains("complete")
                    || normalized.contains("shipped") =>
                {
                    (
                        "GREEN",
                        "0e8a16",
                        format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                    )
                }
                _ if normalized.contains("closed")
                    || normalized.contains("cancel")
                    || normalized.contains("duplicate")
                    || normalized.contains("archive") =>
                {
                    (
                        "GRAY",
                        "6e7781",
                        format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                    )
                }
                _ => (
                    "BLUE",
                    "1f6feb",
                    format!("Lifecycle status `{name}` managed by the Kairastra workflow."),
                ),
            };

            StatusOptionSpec {
                name: name.clone(),
                project_color,
                label_color,
                description,
            }
        })
        .collect()
}

fn push_unique_name(names: &mut Vec<String>, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || is_no_status_change(trimmed) {
        return;
    }
    if names
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(trimmed))
    {
        return;
    }
    names.push(trimmed.to_string());
}

fn is_no_status_change(value: &str) -> bool {
    value
        .trim()
        .eq_ignore_ascii_case("Do not change project status")
}

fn normalize_name(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn url_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{default_label_specs, derive_status_option_names, url_encode};

    #[test]
    fn derive_status_names_preserves_setup_order_and_deduplicates() {
        let names = derive_status_option_names(
            &[
                "Todo".to_string(),
                "In Progress".to_string(),
                "Merging".to_string(),
                "Rework".to_string(),
            ],
            &[
                "Closed".to_string(),
                "Cancelled".to_string(),
                "Duplicate".to_string(),
                "Done".to_string(),
            ],
            &["Todo".to_string()],
            Some("In Progress"),
            Some("Human Review"),
            Some("Done"),
        );

        assert_eq!(
            names,
            vec![
                "Todo",
                "In Progress",
                "Merging",
                "Rework",
                "Closed",
                "Cancelled",
                "Duplicate",
                "Done",
                "Human Review",
            ]
        );
    }

    #[test]
    fn default_label_specs_include_exact_status_names() {
        let specs = default_label_specs(&[
            "Todo".to_string(),
            "Human Review".to_string(),
            "Done".to_string(),
        ]);

        let names = specs.into_iter().map(|spec| spec.name).collect::<Vec<_>>();
        assert!(names.contains(&"kairastra".to_string()));
        assert!(names.contains(&"agent:codex".to_string()));
        assert!(names.contains(&"Human Review".to_string()));
        assert!(!names.contains(&"needs-review".to_string()));
    }

    #[test]
    fn url_encode_handles_agent_labels() {
        assert_eq!(url_encode("agent:codex"), "agent%3Acodex");
    }
}
