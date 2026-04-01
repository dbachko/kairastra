---
name: kairastra-github
description: |
  Use Kairastra's injected `github_graphql` and `github_rest` tools for GitHub
  issue, comment, project, and PR operations during app-server sessions.
---

# GitHub Tracker Tools

Use this skill for raw GitHub tracker operations during Kairastra app-server
sessions.

## Primary tools

- `github_graphql`
- `github_rest`

The app-server injects these tools using Symphony's configured GitHub auth for
the current session.

## Tool contracts

### `github_graphql`

Input:

```json
{
  "query": "query or mutation document",
  "variables": {
    "optional": "graphql variables object"
  }
}
```

Rules:

- Send one GraphQL operation per tool call.
- Treat a top-level `errors` array as a failed operation even if the tool call
  itself succeeded.
- Keep operations narrow; ask only for fields needed for the current step.

### `github_rest`

Input:

```json
{
  "method": "GET | POST | PATCH",
  "path": "/repos/<owner>/<repo>/issues/123/comments",
  "body": {
    "optional": "request payload object"
  }
}
```

Rules:

- The runtime only allow-lists REST paths containing `/issues/` or `/pulls/`.
- Supported methods are `GET`, `POST`, and `PATCH`.
- Prefer `github_graphql` for Project v2 field mutations and structured issue
  reads; use `github_rest` for comments, review comments, issue body edits, and
  PR reads when that is simpler.

## Common workflows

### Query an issue by owner, repo, and number

```graphql
query IssueDetails($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    issue(number: $number) {
      id
      number
      title
      body
      url
      state
      assignees(first: 20) {
        nodes {
          login
        }
      }
      labels(first: 50) {
        nodes {
          name
        }
      }
      projectItems(first: 20) {
        nodes {
          id
          project {
            title
            number
          }
        }
      }
    }
  }
}
```

### List issue comments

```json
{
  "method": "GET",
  "path": "/repos/<owner>/<repo>/issues/<number>/comments"
}
```

### Create an issue comment

```json
{
  "method": "POST",
  "path": "/repos/<owner>/<repo>/issues/<number>/comments",
  "body": {
    "body": "## Codex Workpad\n\n..."
  }
}
```

### Edit an issue body or comment

Edit issue body:

```json
{
  "method": "PATCH",
  "path": "/repos/<owner>/<repo>/issues/<number>",
  "body": {
    "body": "updated issue body"
  }
}
```

Edit an issue comment:

```json
{
  "method": "PATCH",
  "path": "/repos/<owner>/<repo>/issues/comments/<comment_id>",
  "body": {
    "body": "updated comment body"
  }
}
```

### List PR review comments

```json
{
  "method": "GET",
  "path": "/repos/<owner>/<repo>/pulls/<number>/comments"
}
```

### Reply to a PR review comment

```json
{
  "method": "POST",
  "path": "/repos/<owner>/<repo>/pulls/<number>/comments",
  "body": {
    "body": "[codex] Addressing this now.",
    "in_reply_to": 123456789
  }
}
```

### Move a Project item to another status

1. Query the Project field metadata to find the `Status` field id and option ids.
2. Call `updateProjectV2ItemFieldValue` through `github_graphql`.

Example:

```graphql
mutation UpdateProjectStatus(
  $projectId: ID!
  $itemId: ID!
  $fieldId: ID!
  $optionId: String!
) {
  updateProjectV2ItemFieldValue(
    input: {
      projectId: $projectId
      itemId: $itemId
      fieldId: $fieldId
      value: { singleSelectOptionId: $optionId }
    }
  ) {
    projectV2Item {
      id
    }
  }
}
```

## Guidance

- Prefer one persistent workpad comment per issue; edit it in place.
- Keep issue and PR mutations minimal and reviewer-oriented.
- When changing workflow state, read the current issue/project state first so
  you do not stomp newer updates from another actor.
