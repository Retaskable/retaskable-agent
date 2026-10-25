# retaskable-agent

A Rust port of the Retaskable agent. The source of truth is the spec in the Retaskable.com repo:
[docs/specs/agent.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/agent.md). Match the
TypeScript agent (`agent/` there) behaviour for behaviour, and port one piece at a time with its tests.

## Rules

- Windows-first, and it must also run on Linux and macOS. Use `std::path`, never a hardcoded `/` or `\`.
- Keep the same rules as the TypeScript agent:
  - Orchestration uses only `claude -p` or `cursor-agent -p`, plus `git` and the code host's CLI (`gh` or `glab`).
  - There is no DB and no queue.
  - Claude runs with `--permission-mode auto`. Never use `--dangerously-skip-permissions`.
  - State transitions are plain code.
  - The HTTP port serves only `GET /api/health` and the settings page, never a command.
  - It binds to `127.0.0.1`. The cloud link is outbound only.
- Every project has a shared backlog. There's no local mode, and no fallback when the store can't be reached (fail closed).
- Before you commit: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
