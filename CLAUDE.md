# retaskable-agent

The Retaskable agent, in Rust: the only agent since 2026-11-19, when the TypeScript agent (Retaskable.com's `agent/`) and the Electron desktop app were retired. The spec is [docs/specs/agent.md](docs/specs/agent.md) in this repo, and this repo's [docs/specs](docs/specs/README.md) are the source of truth for everything the agent does: change the spec here first, then the code, with its tests. Retaskable.com's [docs/specs](https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs) stay the source for the store, the website and the app; where the agent talks to them (store ops, the hub), follow those specs and change them there.

## Rules

- Windows-first, and it must also run on Linux and macOS. Use `std::path`, never a hardcoded `/` or `\`.
- Keep the same rules as the TypeScript agent:
  - Orchestration uses only `claude -p`, `cursor-agent -p`, `pi` or `codex exec`, plus `git` and the code host's CLI (`gh` or `glab`).
  - There is no DB and no queue.
  - Claude runs with `--permission-mode auto`. Never use `--dangerously-skip-permissions`.
  - Codex sessions always run in Codex's own sandbox (`read-only` or `workspace-write`). Never use `--dangerously-bypass-approvals-and-sandbox` or `danger-full-access`.
  - State transitions are plain code.
  - The HTTP port serves only `GET /api/health` and the settings page, never a command.
  - It binds to `127.0.0.1`. The cloud link is outbound only.
- No release flags in agent code. Ship a new behaviour On and never write its Off path: flags belong to the app and the Worker. A test in `cargo test` (`src/no_flags.rs`) fails on a flag read. See [docs/specs/no-agent-flags.md](docs/specs/no-agent-flags.md).
- Every project has a shared backlog. There's no local mode, and no fallback when the store can't be reached (fail closed).
- Before you commit: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
