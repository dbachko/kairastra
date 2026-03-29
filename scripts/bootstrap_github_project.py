#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.parse
from dataclasses import dataclass
from typing import Any


STATUS_OPTIONS = [
    {
        "name": "Backlog",
        "color": "GRAY",
        "description": "Out of scope until moved into the active queue.",
    },
    {
        "name": "Todo",
        "color": "BLUE",
        "description": "Queued for Kairastra to pick up.",
    },
    {
        "name": "In Progress",
        "color": "YELLOW",
        "description": "Actively being worked by Kairastra.",
    },
    {
        "name": "Human Review",
        "color": "PURPLE",
        "description": "Waiting for human review or approval.",
    },
    {
        "name": "Merging",
        "color": "PINK",
        "description": "Approved and ready for landing.",
    },
    {
        "name": "Rework",
        "color": "ORANGE",
        "description": "Changes requested and work needs another pass.",
    },
    {
        "name": "Done",
        "color": "GREEN",
        "description": "Completed and landed.",
    },
    {
        "name": "Cancelled",
        "color": "RED",
        "description": "Stopped intentionally without completion.",
    },
    {
        "name": "Duplicate",
        "color": "GRAY",
        "description": "Superseded by another issue.",
    },
]

DEFAULT_LABELS = [
    {
        "name": "kairastra",
        "color": "5319e7",
        "description": "Tracked by the Kairastra orchestration workflow.",
    },
    {
        "name": "agent:codex",
        "color": "1f6feb",
        "description": "Assigned to Codex-driven automation.",
    },
    {
        "name": "agent:claude",
        "color": "8250df",
        "description": "Assigned to Claude-driven automation.",
    },
    {
        "name": "agent:gemini",
        "color": "0e8a16",
        "description": "Assigned to Gemini-driven automation.",
    },
    {
        "name": "blocked",
        "color": "d73a4a",
        "description": "Blocked on another task, dependency, or external input.",
    },
    {
        "name": "needs-review",
        "color": "fbca04",
        "description": "Ready for human review.",
    },
    {
        "name": "rework",
        "color": "e99695",
        "description": "Needs another implementation pass.",
    },
]


@dataclass
class Action:
    summary: str
    changed: bool


class BootstrapError(RuntimeError):
    pass


def _project_owner_from_url(raw: str | None) -> str | None:
    if not raw:
        return None
    path = urllib.parse.urlparse(raw).path.strip("/")
    segments = [segment for segment in path.split("/") if segment]
    if len(segments) >= 3 and segments[0] in {"users", "orgs"} and segments[2] == "projects":
        return segments[1]
    return None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Bootstrap a GitHub Project and repo for Kairastra orchestration."
    )
    parser.add_argument(
        "--owner",
        default=os.getenv("KAIRASTRA_GITHUB_OWNER"),
        help="GitHub user or org that owns the Project.",
    )
    parser.add_argument(
        "--repo",
        default=os.getenv("KAIRASTRA_GITHUB_REPO"),
        help="GitHub repository name for issue labels.",
    )
    parser.add_argument(
        "--project-url",
        default=os.getenv("KAIRASTRA_GITHUB_PROJECT_URL"),
        help="GitHub Project URL used to derive the project owner when it differs from the repo owner.",
    )
    parser.add_argument(
        "--project-owner",
        default=os.getenv("KAIRASTRA_GITHUB_PROJECT_OWNER"),
        help="GitHub user or org that owns the Project. Defaults to the parsed project URL owner or --owner.",
    )
    parser.add_argument(
        "--project-number",
        type=int,
        default=_env_int("KAIRASTRA_GITHUB_PROJECT_NUMBER"),
        help="GitHub Project v2 number.",
    )
    parser.add_argument(
        "--priority-field-name",
        default="Priority",
        help="Project field name to ensure for numeric prioritization.",
    )
    parser.add_argument(
        "--status-mode",
        choices=["preserve", "normalize"],
        default="preserve",
        help="Whether to leave the Project Status field unchanged or normalize it to Kairastra defaults.",
    )
    parser.add_argument(
        "--skip-labels",
        action="store_true",
        help="Do not create or update the default Kairastra label pack.",
    )
    parser.add_argument(
        "--skip-priority-field",
        action="store_true",
        help="Do not create the numeric Priority field.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print planned changes without mutating GitHub.",
    )
    parser.add_argument(
        "--confirm-normalize-token",
        help="Internal use: bypass the interactive destructive confirmation when the exact expected token is provided.",
    )
    args = parser.parse_args()
    if not args.project_owner:
        args.project_owner = _project_owner_from_url(args.project_url) or args.owner

    missing = [
        name
        for name, value in (
            ("--owner / KAIRASTRA_GITHUB_OWNER", args.owner),
            ("--project-owner / KAIRASTRA_GITHUB_PROJECT_OWNER", args.project_owner),
            ("--repo / KAIRASTRA_GITHUB_REPO", args.repo),
            ("--project-number / KAIRASTRA_GITHUB_PROJECT_NUMBER", args.project_number),
        )
        if not value
    ]
    if missing:
        parser.error(f"missing required settings: {', '.join(missing)}")

    return args


def _env_int(name: str) -> int | None:
    value = os.getenv(name)
    if not value:
        return None
    try:
        return int(value)
    except ValueError as exc:
        raise BootstrapError(f"{name} must be an integer, got {value!r}") from exc


def run_json(command: list[str], payload: dict[str, Any] | None = None) -> Any:
    result = subprocess.run(
        command,
        input=json.dumps(payload) if payload is not None else None,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        stderr = result.stderr.strip()
        raise BootstrapError(
            f"command failed ({' '.join(command)}): {stderr or result.stdout.strip()}"
        )
    stdout = result.stdout.strip()
    return json.loads(stdout) if stdout else None


def run(command: list[str]) -> None:
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    if result.returncode != 0:
        stderr = result.stderr.strip()
        raise BootstrapError(
            f"command failed ({' '.join(command)}): {stderr or result.stdout.strip()}"
        )


def gh_graphql(query: str, variables: dict[str, Any] | None = None) -> Any:
    return run_json(
        ["gh", "api", "graphql", "--input", "-"],
        {"query": query, "variables": variables or {}},
    )


def load_project(project_number: int, owner: str) -> dict[str, Any]:
    return run_json(
        ["gh", "project", "view", str(project_number), "--owner", owner, "--format", "json"]
    )


def load_fields(project_number: int, owner: str) -> list[dict[str, Any]]:
    payload = run_json(
        ["gh", "project", "field-list", str(project_number), "--owner", owner, "--format", "json"]
    )
    return payload.get("fields", [])


def ensure_status_field(
    *,
    owner: str,
    project_number: int,
    fields: list[dict[str, Any]],
    dry_run: bool,
) -> Action:
    desired_names = [option["name"] for option in STATUS_OPTIONS]
    status_field = next((field for field in fields if field.get("name") == "Status"), None)

    if status_field is None:
        summary = f"create project Status field with options: {', '.join(desired_names)}"
        if not dry_run:
            run(
                [
                    "gh",
                    "project",
                    "field-create",
                    str(project_number),
                    "--owner",
                    owner,
                    "--name",
                    "Status",
                    "--data-type",
                    "SINGLE_SELECT",
                    "--single-select-options",
                    ",".join(desired_names),
                ]
            )
        return Action(summary, True)

    current_names = [option.get("name") for option in status_field.get("options", [])]
    if current_names == desired_names:
        return Action("Status field already matches the Kairastra workflow states.", False)

    summary = (
        "update Status field options from "
        f"{current_names!r} to {desired_names!r}"
    )
    if not dry_run:
        gh_graphql(
            """
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
            """,
            {
                "fieldId": status_field["id"],
                "name": "Status",
                "options": STATUS_OPTIONS,
            },
        )
    return Action(summary, True)


def load_project_status_counts(project_number: int, owner: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    after: str | None = None

    while True:
        payload = gh_graphql(
            """
            query ProjectStatusItems($owner: String!, $projectNumber: Int!, $after: String) {
              organization(login: $owner) {
                projectV2(number: $projectNumber) {
                  items(first: 100, after: $after) {
                    pageInfo {
                      hasNextPage
                      endCursor
                    }
                    nodes {
                      status: fieldValueByName(name: "Status") {
                        __typename
                        ... on ProjectV2ItemFieldSingleSelectValue { name }
                        ... on ProjectV2ItemFieldTextValue { text }
                        ... on ProjectV2ItemFieldNumberValue { number }
                      }
                    }
                  }
                }
              }
              user(login: $owner) {
                projectV2(number: $projectNumber) {
                  items(first: 100, after: $after) {
                    pageInfo {
                      hasNextPage
                      endCursor
                    }
                    nodes {
                      status: fieldValueByName(name: "Status") {
                        __typename
                        ... on ProjectV2ItemFieldSingleSelectValue { name }
                        ... on ProjectV2ItemFieldTextValue { text }
                        ... on ProjectV2ItemFieldNumberValue { number }
                      }
                    }
                  }
                }
              }
            }
            """,
            {
                "owner": owner,
                "projectNumber": project_number,
                "after": after,
            },
        )
        project = (
            payload.get("data", {}).get("organization", {}).get("projectV2")
            or payload.get("data", {}).get("user", {}).get("projectV2")
        )
        if not project:
            break
        items = project.get("items", {})
        for node in items.get("nodes", []):
            status = node.get("status") or {}
            value = status.get("name") or status.get("text") or status.get("number")
            if value is None:
                continue
            rendered = str(value)
            counts[rendered] = counts.get(rendered, 0) + 1
        page_info = items.get("pageInfo", {})
        if not page_info.get("hasNextPage"):
            break
        after = page_info.get("endCursor")
    return counts


def normalization_block_reason(status_counts: dict[str, int]) -> str | None:
    if not status_counts:
        return None
    desired = {option["name"].lower() for option in STATUS_OPTIONS}
    incompatible = sorted(
        status for status, count in status_counts.items() if count > 0 and status.lower() not in desired
    )
    if not incompatible:
        return None
    return (
        "normalization is blocked because this Project already has items in statuses that would be changed or removed: "
        + ", ".join(incompatible)
    )


def confirm_status_normalization(
    *,
    owner: str,
    project_number: int,
    provided_token: str | None,
) -> None:
    expected = f"normalize {owner}#{project_number}"
    if provided_token is not None:
        if provided_token != expected:
            raise BootstrapError("invalid --confirm-normalize-token")
        return
    if not sys.stdin.isatty():
        raise BootstrapError("status normalization requires an interactive terminal")

    print()
    print("Normalize GitHub Project Status field?")
    print(
        f"This will update the Status field on GitHub Project {owner}#{project_number} to Kairastra's default options."
    )
    print("Status options that are not in the target set will be removed from the field definition.")
    print("Kairastra cannot undo this change.")
    typed = input(f"To continue, type: {expected}\n> ").strip()
    if typed != expected:
        raise BootstrapError("status normalization confirmation did not match")


def ensure_priority_field(
    *,
    owner: str,
    project_number: int,
    field_name: str,
    fields: list[dict[str, Any]],
    dry_run: bool,
) -> Action:
    if any(field.get("name") == field_name for field in fields):
        return Action(f"{field_name} field already exists.", False)

    summary = f"create numeric project field {field_name!r}"
    if not dry_run:
        run(
            [
                "gh",
                "project",
                "field-create",
                str(project_number),
                "--owner",
                owner,
                "--name",
                field_name,
                "--data-type",
                "NUMBER",
            ]
        )
    return Action(summary, True)


def list_labels(owner: str, repo: str) -> dict[str, dict[str, Any]]:
    page = 1
    labels: dict[str, dict[str, Any]] = {}
    while True:
        response = run_json(
            [
                "gh",
                "api",
                f"repos/{owner}/{repo}/labels?per_page=100&page={page}",
            ]
        )
        if not response:
            break
        for label in response:
            labels[label["name"].lower()] = label
        page += 1
    return labels


def ensure_labels(owner: str, repo: str, dry_run: bool) -> list[Action]:
    existing = list_labels(owner, repo)
    actions: list[Action] = []

    for label in DEFAULT_LABELS:
        current = existing.get(label["name"].lower())
        if current is None:
            summary = f"create repo label {label['name']!r}"
            if not dry_run:
                run(
                    [
                        "gh",
                        "api",
                        f"repos/{owner}/{repo}/labels",
                        "--method",
                        "POST",
                        "-f",
                        f"name={label['name']}",
                        "-f",
                        f"color={label['color']}",
                        "-f",
                        f"description={label['description']}",
                    ]
                )
            actions.append(Action(summary, True))
            continue

        current_color = current.get("color", "").lower()
        current_description = current.get("description") or ""
        if current_color == label["color"].lower() and current_description == label["description"]:
            actions.append(Action(f"label {label['name']!r} already matches.", False))
            continue

        summary = f"update repo label {label['name']!r}"
        if not dry_run:
            encoded_name = urllib.parse.quote(label["name"], safe="")
            run(
                [
                    "gh",
                    "api",
                    f"repos/{owner}/{repo}/labels/{encoded_name}",
                    "--method",
                    "PATCH",
                    "-f",
                    f"new_name={label['name']}",
                    "-f",
                    f"color={label['color']}",
                    "-f",
                    f"description={label['description']}",
                ]
            )
        actions.append(Action(summary, True))

    return actions


def main() -> int:
    args = parse_args()

    project = load_project(args.project_number, args.project_owner)
    fields = load_fields(args.project_number, args.project_owner)
    status_counts = load_project_status_counts(args.project_number, args.project_owner)
    block_reason = normalization_block_reason(status_counts)

    if args.status_mode == "normalize" and block_reason and not args.dry_run:
        raise BootstrapError(
            block_reason
            + ". Kairastra will not rewrite a live Project without an explicit migration feature."
        )
    if args.status_mode == "normalize" and not args.dry_run:
        confirm_status_normalization(
            owner=args.project_owner,
            project_number=args.project_number,
            provided_token=args.confirm_normalize_token,
        )

    actions = []
    if args.status_mode == "normalize":
        actions.append(
            ensure_status_field(
                owner=args.project_owner,
                project_number=args.project_number,
                fields=fields,
                dry_run=args.dry_run,
            )
        )
    else:
        actions.append(Action("Status field left unchanged (preserve mode).", False))

    if not args.skip_priority_field:
        actions.append(
            ensure_priority_field(
                owner=args.project_owner,
                project_number=args.project_number,
                field_name=args.priority_field_name,
                fields=fields,
                dry_run=args.dry_run,
            )
        )

    if not args.skip_labels:
        actions.extend(ensure_labels(args.owner, args.repo, args.dry_run))

    changed = [action.summary for action in actions if action.changed]
    unchanged = [action.summary for action in actions if not action.changed]

    print(f"Project: {project['title']} ({args.project_owner}#{args.project_number})")
    print(f"Repository: {args.owner}/{args.repo}")
    print(f"Mode: {'dry-run' if args.dry_run else 'apply'}")
    print(f"Status field mode: {args.status_mode}")
    print()

    if changed:
        print("Changes:")
        for summary in changed:
            print(f"- {summary}")
    else:
        print("Changes:")
        print("- none")

    if unchanged:
        print()
        print("Already satisfied:")
        for summary in unchanged:
            print(f"- {summary}")

    print()
    print(
        "Recommended Project status options: "
        + ", ".join(option["name"] for option in STATUS_OPTIONS)
    )
    if block_reason:
        print(block_reason + ".")
    print(
        "Dispatchable workflow states stay narrower: Todo, In Progress, Merging, Rework."
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BootstrapError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
