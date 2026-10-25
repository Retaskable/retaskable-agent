# retaskable-agent

The Retaskable agent, rewritten in Rust. It will replace the TypeScript agent in
[Retaskable.com's `agent/`](https://github.com/Retaskable/Retaskable.com/tree/main/agent). It follows the same spec,
[docs/specs/agent.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/agent.md), and is ported piece
by piece.

## What it does so far

- It listens on `127.0.0.1`, never a public port.
  - The port is `RETASKABLE_PORT` if set, then `port` in `config.json`, then 4700.
- `GET /api/health` answers `{"ok": true, "version": "..."}`. Every other path is a 404.
- It reads `config.json` from `RETASKABLE_DATA_DIR`, or from `.retaskable/` in the working folder.
- Logging is set by `RETASKABLE_LOG` (default `info`).

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
