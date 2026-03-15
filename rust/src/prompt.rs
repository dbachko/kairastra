use anyhow::{Context, Result};
use liquid::object;

use crate::model::Issue;
use crate::workflow::WorkflowSnapshot;

pub fn build_prompt(
    snapshot: &WorkflowSnapshot,
    issue: &Issue,
    attempt: Option<u32>,
) -> Result<String> {
    let parser = liquid::ParserBuilder::with_stdlib()
        .build()
        .context("template_parse_error")?;
    let template_text = snapshot.settings.workflow_prompt(&snapshot.definition);
    let template = parser
        .parse(&template_text)
        .with_context(|| format!("template_parse_error: {template_text}"))?;

    let blockers: Vec<liquid::model::Value> = issue
        .blocked_by
        .iter()
        .map(|blocker| {
            object!({
                "id": blocker.id.clone(),
                "identifier": blocker.identifier.clone(),
                "state": blocker.state.clone(),
            })
            .into()
        })
        .collect();

    let globals = object!({
        "tracker": {
            "kind": snapshot.settings.tracker.kind.clone(),
            "mode": match snapshot.settings.tracker.mode {
                crate::config::GitHubMode::ProjectsV2 => "projects_v2",
                crate::config::GitHubMode::IssuesOnly => "issues_only",
            },
            "owner": snapshot.settings.tracker.owner.clone(),
            "repo": snapshot.settings.tracker.repo.clone(),
            "project_v2_number": snapshot.settings.tracker.project_v2_number,
            "project_url": snapshot.settings.tracker.project_url.clone(),
            "dashboard_url": snapshot.settings.tracker_dashboard_url(),
        },
        "attempt": attempt,
        "issue": {
            "id": issue.id.clone(),
            "identifier": issue.identifier.clone(),
            "title": issue.title.clone(),
            "description": issue.description.clone(),
            "priority": issue.priority,
            "state": issue.state.clone(),
            "branch_name": issue.branch_name.clone(),
            "url": issue.url.clone(),
            "workpad_comment_id": issue.workpad_comment_id,
            "workpad_comment_url": issue.workpad_comment_url.clone(),
            "assignees": issue.assignees.clone(),
            "labels": issue.labels.clone(),
            "blocked_by": blockers,
            "created_at": issue.created_at.map(|value| value.to_rfc3339()),
            "updated_at": issue.updated_at.map(|value| value.to_rfc3339()),
        }
    });

    template.render(&globals).context("template_render_error")
}

pub fn continuation_prompt(_issue: &Issue, turn_number: usize, max_turns: usize) -> String {
    format!(
        "Continuation guidance:\n\n- The previous Codex turn completed normally, but the GitHub issue is still in an active state.\n- This is continuation turn #{turn_number} of {max_turns} for the current agent run.\n- Resume from the current workspace and workpad state instead of restarting from scratch.\n- The original task instructions and prior turn context are already present in this thread, so do not restate them before acting.\n- Focus on the remaining ticket work and do not end the turn while the issue stays active unless you are truly blocked.\n"
    )
}

#[cfg(test)]
mod tests {
    use crate::config::Settings;
    use crate::model::{Issue, WorkflowDefinition};
    use crate::workflow::WorkflowSnapshot;

    use super::build_prompt;

    #[test]
    fn renders_issue_fields_into_prompt() {
        let workflow = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: openai
  project_v2_number: 7
  api_key: fake
"#,
            )
            .unwrap(),
            prompt_template: "Issue {{ issue.identifier }}: {{ issue.title }}".to_string(),
        };
        let settings = Settings::from_workflow(&workflow).unwrap();
        let snapshot = WorkflowSnapshot {
            definition: workflow,
            settings,
        };

        let issue = Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "openai/repo#7".to_string(),
            title: "Implement Rust version".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
        };

        let prompt = build_prompt(&snapshot, &issue, None).unwrap();
        assert!(prompt.contains("openai/repo#7"));
        assert!(prompt.contains("Implement Rust version"));
    }

    #[test]
    fn default_prompt_renders_tracker_dashboard_url() {
        let workflow = WorkflowDefinition {
            config: serde_yaml::from_str(
                r#"
tracker:
  kind: github
  owner: dbachko
  project_v2_number: 7
  project_url: https://github.com/users/dbachko/projects/7
  api_key: fake
"#,
            )
            .unwrap(),
            prompt_template: String::new(),
        };
        let settings = Settings::from_workflow(&workflow).unwrap();
        let snapshot = WorkflowSnapshot {
            definition: workflow,
            settings,
        };

        let issue = Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "dbachko/symphony-gh#1".to_string(),
            title: "Dashboard prompt".to_string(),
            description: Some("body".to_string()),
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: Some("https://github.com/dbachko/symphony-gh/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: None,
            workpad_comment_url: None,
        };

        let prompt = build_prompt(&snapshot, &issue, None).unwrap();
        assert!(prompt.contains("GitHub dashboard: https://github.com/users/dbachko/projects/7"));
    }
}
