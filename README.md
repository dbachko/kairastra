# Symphony

Symphony turns project work into isolated, autonomous implementation runs, allowing teams to manage
work instead of supervising coding agents.

[![Symphony demo video preview](.github/media/symphony-demo-poster.jpg)](.github/media/symphony-demo.mp4)

_In this [demo video](.github/media/symphony-demo.mp4), Symphony monitors a work queue and spawns agents to handle the tasks. The agents complete the tasks and provide proof of work: CI status, PR review feedback, complexity analysis, and walkthrough videos. Engineers do not need to supervise Codex; they can manage the work at a higher level._

> [!WARNING]
> Symphony is a low-key engineering preview for testing in trusted environments.

## Running Symphony

### Requirements

Symphony works best in codebases that have adopted
[harness engineering](https://openai.com/index/harness-engineering/). Symphony is the next step --
moving from managing coding agents to managing work that needs to get done.

### Option 1. Make your own

Tell your favorite coding agent to build Symphony in a programming language of your choice:

> Implement Symphony according to the following spec:
> https://github.com/openai/symphony/blob/main/SPEC.md

### Option 2. Use the Rust implementation in this repo

Check out [rust/README.md](rust/README.md) for setup and run instructions for the current
GitHub-oriented Symphony implementation. It uses GitHub Issues and Projects v2, treats a GitHub
Project as the primary dashboard when configured, and runs Codex through the app-server protocol.
The Rust binary now also includes `setup`, `doctor`, and `auth` subcommands for operator-oriented
VPS and Docker onboarding. That guide also documents the GitHub token requirement for Project v2
workflows, including the need for a classic PAT on user-owned projects and `workflow` scope when
agent branches may modify GitHub Actions files.
You can also ask your favorite coding agent to help with the setup:

> Set up Symphony for my repository based on
> https://github.com/openai/symphony/blob/main/rust/README.md

---

## License

This project is licensed under the [Apache License 2.0](LICENSE).
