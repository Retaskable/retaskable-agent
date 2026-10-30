# retaskable-agent

The Retaskable agent, rewritten in Rust. It will replace the TypeScript agent in
[Retaskable.com's `agent/`](https://github.com/Retaskable/Retaskable.com/tree/main/agent). It follows the same spec, trimmed to what belongs on the agent:
[docs/specs/agent.md](docs/specs/agent.md). Those specs are a snapshot of [Retaskable.com's](https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs), which stays the source until this agent replaces the TypeScript one ([docs/specs/README.md](docs/specs/README.md)). It is ported piece
by piece.

## What it does so far

- It listens on `127.0.0.1`, never a public port.
  - The port is `RETASKABLE_PORT` if set, then `port` in `config.json`, then 4700.
- `GET /api/health` answers `{"ok": true, "version": "..."}`. Every other path is a 404.
- It reads `config.json` from `RETASKABLE_DATA_DIR`, or from `.retaskable/` in the working folder.
- Logging is set by `RETASKABLE_LOG` (default `info`).
- `RETASKABLE_ORCHESTRATOR` switches the orchestrator loop: `off` (the default), `dry` or `claim`. `dry` reads each project's backlog once a minute and writes a `tick` event to `projects/<id>/events/*.jsonl` with `status`, `states` and `dry` (what a tick would do); it never claims, takes a lane or starts a session. `claim` claims work, starts a real Claude implementer session in a lane (`orchestrator/session.rs`; any other engine refuses and gives the item back), and stops there: a session that finished parks the item at `checking`, holding its lane and lease, and one that crashed or gave up hands the item back to the queue. Claim mode is for staging tests until checks, CI, push and merge are built. It still runs dry while a TypeScript agent answers on this computer. An unreachable store is a `store_error` event and a skipped tick.

## Run

```sh
cargo run
curl http://127.0.0.1:4700/api/health
```

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
