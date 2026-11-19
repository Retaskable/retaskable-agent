# retaskable-agent

The Retaskable agent, in Rust. Since 2026-11-19 it is the only agent: the TypeScript agent that was in
Retaskable.com's `agent/` has been retired. Its spec is [docs/specs/agent.md](docs/specs/agent.md), and this repo's
[docs/specs](docs/specs/README.md) are the source of truth for the agent.

## What it does so far

- It listens on `127.0.0.1`, never a public port.
- It reads no release flags: a new behaviour ships On, with no Off path (`src/no_flags.rs` fails `cargo test` on a flag read).
  - The port is `RETASKABLE_PORT` if set, then `port` in `config.json`, then 4700.
- `GET /api/health` answers `{"ok": true, "version": "..."}`. Every other path is a 404.
- It reads `config.json` from `RETASKABLE_DATA_DIR`, or from `.retaskable/` in the working folder.
- Logging is set by `RETASKABLE_LOG` (default `info`).
- `RETASKABLE_ORCHESTRATOR` switches the orchestrator loop: `off` (the default), `dry` or `claim`. `dry` reads each project's backlog once a minute and writes a `tick` event to `projects/<id>/events/*.jsonl` with `status`, `states` and `dry` (what a tick would do); it never claims, takes a lane or starts a session. `claim` claims work, starts a real Claude implementer session in a lane (`orchestrator/session.rs`; any other engine refuses and gives the item back), and stops there: a session that finished parks the item at `checking`, holding its lane and lease, one that gave up or needs a person goes to `needs_human`, and one that crashed hands the item back to the queue. Claim mode is for staging tests until checks, CI, push and merge are built. It still runs dry while a TypeScript agent answers on this computer. An unreachable store is a `store_error` event and a skipped tick.

## Install

**Binaries.** Each [release](https://github.com/Retaskable/retaskable-agent/releases/latest) has `retaskable`,
`retaskable-ci` and `retaskable-chat-agent` for `linux`, `darwin` and `win32`, `x64` and `arm64`, named
`<program>-<platform>-<arch>` (`.exe` on Windows), each with a `.sha256` file holding the hex digest. Check it before
running:

```sh
base=https://github.com/Retaskable/retaskable-agent/releases/latest/download
curl -fLO $base/retaskable-linux-x64 && curl -fLO $base/retaskable-linux-x64.sha256
echo "$(cat retaskable-linux-x64.sha256)  retaskable-linux-x64" | sha256sum -c -   # macOS: shasum -a 256 -c -
chmod +x retaskable-linux-x64
```

The binary needs these on the machine ([docs/specs/docker.md](docs/specs/docker.md) section 2 lists what the image
carries): `git`; `gh` (GitHub) or `glab` (GitLab); the Claude Code CLI (`claude`), or whichever of `cursor-agent`, `pi`
or `codex` you use; and whatever your projects' own checks need (Node, Rust, ...). On Linux the tray icon needs
`libayatana-appindicator3-1` (see below). The binaries are not code-signed, so Windows SmartScreen and macOS
Gatekeeper may ask first.

**Docker.** Images for linux/amd64 and linux/arm64, with no secret in them, are published to GHCR on each release:
`ghcr.io/retaskable/retaskable-agent` and `ghcr.io/retaskable/retaskable-ci`, tagged with the version (no `v`) and
`latest`. The compose files pull them by default: [docker/agent](docker/agent/README.md) and
[docker/ci](docker/ci/README.md).

## Run

```sh
cargo run
curl http://127.0.0.1:4700/api/health
```

## Tray icon on Linux

The tray helper (`systray2`'s prebuilt one) draws its icon through AppIndicator, so a Linux desktop needs
`libayatana-appindicator` (Debian and Ubuntu: `libayatana-appindicator3-1`). Without it, or without a tray,
the agent runs headless. A release bundle ships the helper beside the executable; in a checkout, set
`RETASKABLE_TRAY_HELPER` to one.

## Check

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs these on Windows, Linux and macOS.

## Licence

[Apache-2.0](LICENSE). The source is public so you can see what you're installing and what it sends to Retaskable's
servers, and there are no secrets in it. The licence doesn't cover the Retaskable name: see [NOTICE](NOTICE).
