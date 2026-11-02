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



### 8.1 What the Rust agent does today (as built)

Each engine has its own `summarize(event, at)` (`engines/summarize.rs` for Claude Code, `engines/cursor/result.rs`, `engines/pi.rs`, `engines/codex.rs`); `Engine::summarize` picks it, and `engines/transcripts.rs` reads a run's folder back with it.

- **The file.** `engines/runner.rs`'s `open_sink` creates `<run folder>/<sessionId>.jsonl` and writes one `retaskable_meta` line first: `model`, `engine` (`claude`, `cursor`, `pi` or `codex`), `at` (epoch milliseconds) and, for an implementer, `lane`. Every raw line the engine prints is then appended as it arrives. Nothing is rewritten.
- **Reading it back.** `read_transcripts` takes every `.jsonl` in the run's folder and orders them by the meta line's `at` (the file's modified time when there is none). A transcript opens with a `result` event reading `— <role> on <model> —`, then each later line goes through the summarizer of the engine the meta line names. A missing or unknown `engine` is Claude Code. A line that isn't JSON is skipped. A transcript that can't be opened is skipped, not an error.
- **Every event in a stored transcript carries the file's start time as its `at`**, because the raw lines hold no time of their own; only the order tells you what came after what.
- **Text is clipped.** A `text` event is trimmed and cut at 800 characters; a `tool` event's argument has its whitespace collapsed to single spaces and is cut at 160, both ending in `…` when cut. A blank text block gives no event.
- **The same three kinds on every engine.** `tool`, `text` and `result` are all the drawer knows. Each engine's table below is its whole mapping; anything not in it gives no event and stays in the file.

| Engine | `text` | `tool` | `result` |
|---|---|---|---|
| Claude Code | `assistant` text blocks | `assistant` `tool_use`: `<Tool>: <first of command, file_path, notebook_path, pattern, url, query, description, prompt>`; `StructuredOutput` is always `Result: structured output`, never its input | `result`: `Finished: done` on `success`, else `Finished: <subtype>` |
| Cursor | as the table above | as the table above, plus a completed or errored `shellToolCall` the allowlist refused: `Bash refused: <reason>` (200 characters; an empty reason says the command is not on the allowlist, Shell matches the first word only) | as above; an `is_error` result is `Finished: <subtype>` even when the subtype says success |
| pi | an assistant `message_end`'s text | `tool_execution_start` and `tool_execution_end`: the first of `command`, `path`, `pattern` alone (no tool name), else the tool's name | `auto_retry_start` is `Retrying`; `agent_end` is `Retrying` when `willRetry` is true, else `Finished` |
| Codex | an `agent_message` when it completes | `command_execution` when it starts (the command, whitespace collapsed); `file_change` when it completes: `Edited <paths>`; `mcp_tool_call`: `<server>.<tool>`; `web_search`: `Search: <query>` | `turn.completed` is `Finished`; `turn.failed` is `Failed: <message>` (160 characters) |

- **Known gaps in what is built.** pi's `tool` text drops the tool name when it has an argument, and a tool that starts and ends shows twice. The runner's meta line does not yet carry `role` or `scout`, so a transcript header reads `— session on <model> —` and the drawer can't tell an implementer from a reviewer. Both are for the **[Proposal]** below.

### 8.2 Proposals: what the Now tab and the site need

Both are **[Proposal]**s, for the human to decide. They are what `live-sessions-reporting` (the Now tab and the cloud tick's session count) and the watched transcripts need beyond today's reading of a finished file.

- **[Proposal] The meta line says who the session is.** `retaskable_meta` gains `role` (`implementer`, `reviewer`, `planner`, `scout`, `triage`, `draft`, `chat`, `release_notes`, `ideas`, `look`) and, for a scout, `scout` (its name), `item` (the item's id) and, under runs ([pieces.md](pieces.md)), `piece` (the piece's id). `read_transcripts` already reads `role` and `scout`; this only makes the runner write them. A file without them reads as today.
- **[Proposal] Real times.** The summarizer is given the time the runner read the line, not the file's start. the raw lines are kept as the engine printed them, so a time can't live in the file without changing it; instead a live watch (below) stamps events as they are read, and a stored transcript keeps its single start time and says so ("times are the session's start"). Re-timestamping stored lines is out of scope.
- **[Proposal] Live means tailing the file.** A session is *live* while its `.jsonl` is being appended and its process tree (or, for a detached implementer, the process id in `<session>.live.json`, [agent.md](agent.md) section 7.4) is alive. The Now tab lists each live session with its role, engine, model, item and piece, how long it has run, and the last `tool` or `text` event, read from the tail of the file (the last 64 KB, so a long run is never read whole). The cloud tick's session count is the number of those, not the number of lanes in use.
- **[Proposal] Bounded watching.** `session_events` carries at most the last 200 events on a `watch` and then only the new ones; `unwatch`, or 10 minutes without a `watch` refresh, stops it. A session that ends sends its `result` event last. Nothing is stored in the cloud, as today.
- **[Proposal] A file can end without a result.** A transcript whose last event isn't `result` and whose process is gone ended without one (killed, or the agent restarted: [agent.md](agent.md) section 7.4). The drawer shows it as "Ended without a result"; a session cut by a restart is `interrupted` and is picked up again in a new file, so one item can have several transcripts in a row.
- **[Proposal] Privacy.** Transcripts hold the user's code and commands, so they stay on the computer (the README's first rule here). A `watch` is answered only from the computer's own files, and `session_events` text is the clipped summary above, never a raw line.

### 8.3 Tests

- Built: each engine's summarizer from recorded lines (`summarize.rs`, `cursor/result.rs`, `pi.rs` and `codex.rs` tests), and `transcripts.rs`'s tests for the order, the header, a non-JSON line, a folder with no run and a non-`.jsonl` file.
- **[Proposal]** For 8.2: a meta line with `role` and `scout` round-trips through `read_transcripts`; a file with no `role` still reads; a tail read of a file larger than 64 KB returns only complete lines; a transcript with no `result` and a dead process reads as ended without one; a `watch` that isn't refreshed stops after 10 minutes.
