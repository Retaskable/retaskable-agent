# retaskable-agent

A Rust port of the Retaskable agent. The spec is [docs/specs/agent.md](docs/specs/agent.md) in this repo: a snapshot of the Retaskable.com specs, trimmed to what belongs on the agent ([docs/specs/README.md](docs/specs/README.md), [docs/specs/TRIMMED.md](docs/specs/TRIMMED.md)). Retaskable.com's [docs/specs](https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs) stays the source of truth until this agent replaces the TypeScript one, so change behaviour there first and copy it here. Match the
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
