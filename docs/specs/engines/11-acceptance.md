## 11. Acceptance

- With no `engines` in the config, every session runs exactly as before slice 1.
- A project with the implementer on Cursor opens PRs from `cursor-agent` sessions, reviewed and merged by Claude Code sessions, through the same steps; its transcripts read like a Claude session's wherever they are watched.
- No Cursor session is ever started with `--force`, and none starts under an allowlist allowing `Shell(*)`.
- A Cursor result that's missing or fails its schema is asked for again, then fails the session like a missing structured output; it never reaches a step unchecked.
- While Claude Code waits for the limit, Cursor roles carry on.
- A Cursor session's cost is reported as unknown (`null`), never $0.
- With `cursor.models.implementer` set to `composer-2`, an implementer session's arguments include `--model composer-2`, and a planner session on Cursor includes `--model` of the `top` name. An empty box is `auto`. A name with a space is refused when Settings saves.
- A project whose check is `npm test` writes `Shell(npm)` into a writing lane's allowlist. `curl` in a check adds nothing, and the orchestrator still runs that check itself. `Shell(*)` can't be added. A repo that tracks `.cursor/cli.json` keeps that file byte for byte, and Settings says the extra entries don't apply.

