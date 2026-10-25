## 8. Live transcripts

Built (`summarizeCursor`). Cursor's `--output-format stream-json` writes one JSON object a line, as Claude Code's does. The raw lines go to the run's `.jsonl` as today, after a `retaskable_meta` line with `engine: "cursor"`, and `summarizeCursor` turns them into the same `SessionEvent`s the Agents tab, the item drawer and retaskable.com's watched transcripts already show:

| Cursor event | `SessionEvent` |
|---|---|
| `system` / `init` (with `session_id`, `model`) | none; the chat id and model are recorded |
| `assistant`, text content | `text` |
| `tool_call` `started`, shell (`shellToolCall`) | `tool`: `Bash: <command>` |
| `tool_call` `started`, read / edit / write / grep / glob | `tool`: `Read: <path>`, `Edit: <path>`, `Write: <path>`, `Grep: <pattern>`, `Glob: <pattern>` |
| `tool_call` `started`, anything else | `tool`: `<tool name>` |
| `tool_call` `completed` | none (as Claude Code's tool results today) |
| `result` | `result`: `Finished: done` or `Finished: <subtype>` |

- Lines that don't parse, and event types not in the table, are kept in the `.jsonl` and shown as nothing, so a newer `cursor-agent` adding events breaks nothing.
- The tool names are mapped to Claude Code's (`Bash`, `Read`, `Edit`, `Write`, `Grep`, `Glob`), so the transcript reads the same whichever engine ran it.
- `session_events`, `watch` and `unwatch` ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 5.2) don't change: they carry `SessionEvent`s. Nothing is stored in the cloud.
- Tested in `cursor.test.ts` from a recorded run's lines.

