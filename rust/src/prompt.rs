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
            "workpad_comment_body": issue.workpad_comment_body.clone(),
            "assignees": issue.assignees.clone(),
            "labels": issue.labels.clone(),
            "blocked_by": blockers,
            "created_at": issue.created_at.map(|value| value.to_rfc3339()),
            "updated_at": issue.updated_at.map(|value| value.to_rfc3339()),
        }
    });

    template.render(&globals).context("template_render_error")
}

pub fn continuation_prompt(issue: &Issue, turn_number: usize, max_turns: usize) -> String {
    let mut guidance = format!(
        "Continuation guidance:\n\n- The previous Codex turn completed normally, but GitHub issue `{}` is still in active state `{}`.\n- This is continuation turn #{turn_number} of {max_turns} for the current agent run.\n- Resume from the current workspace and workpad state instead of restarting from scratch.\n- The original task instructions and prior turn context are already present in this thread, so do not restate them before acting.\n- Focus on the remaining issue work and do not end the turn while the issue stays active unless you are truly blocked.\n",
        issue.identifier, issue.state
    );

    if let Some(url) = issue.workpad_comment_url.as_deref() {
        guidance.push_str(&format!(
            "- The persistent workpad comment for this issue is `{url}`. Reuse and edit that exact comment.\n"
        ));
    }

    if let Some(body) = issue.workpad_comment_body.as_deref() {
        guidance.push_str("\nCurrent workpad body:\n\n```md\n");
        guidance.push_str(body);
        guidance.push_str("\n```\n");

        if body.contains("Bootstrap created by Symphony runtime before the first Codex turn.")
            || !body.contains("[x]")
        {
            guidance.push_str(
                "\n- The workpad is still bootstrap-only or otherwise not reconciled. Your first action this turn must be to update that existing workpad comment with real plan/checklist progress before any further implementation or review handoff work.\n",
            );
        }
    }

    guidance
}

#[cfg(test)]
mod tests {
    use crate::config::Settings;
    use crate::model::{Issue, WorkflowDefinition};
    use crate::workflow::WorkflowSnapshot;

    use super::{build_prompt, continuation_prompt};

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
            workpad_comment_body: None,
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
            workpad_comment_body: None,
        };

        let prompt = build_prompt(&snapshot, &issue, None).unwrap();
        assert!(prompt.contains("GitHub dashboard: https://github.com/users/dbachko/projects/7"));
    }

    #[test]
    fn continuation_prompt_calls_out_bootstrap_only_workpad() {
        let issue = Issue {
            id: "1".to_string(),
            project_item_id: None,
            identifier: "dbachko/symphony-gh#1".to_string(),
            title: "Continuation".to_string(),
            description: None,
            priority: None,
            state: "In Progress".to_string(),
            branch_name: None,
            url: Some("https://github.com/dbachko/symphony-gh/issues/1".to_string()),
            assignees: Vec::new(),
            labels: Vec::new(),
            blocked_by: Vec::new(),
            created_at: None,
            updated_at: None,
            workpad_comment_id: Some(99),
            workpad_comment_url: Some(
                "https://github.com/dbachko/symphony-gh/issues/1#issuecomment-99".to_string(),
            ),
            workpad_comment_body: Some(
                "## Codex Workpad\n\n### Notes\n\n- Bootstrap created by Symphony runtime before the first Codex turn.\n"
                    .to_string(),
            ),
        };

        let prompt = continuation_prompt(&issue, 2, 20);
        assert!(prompt.contains("first action this turn must be to update"));
        assert!(prompt.contains("issuecomment-99"));
    }
}
