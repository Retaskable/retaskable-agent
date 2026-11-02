## 2. The engine layer

What was `agent/src/claude.ts` is now `agent/src/engines/`, and every importer (`steps.ts`, `setup.ts`, `releaseNotes.ts`, `playtestGroups.ts`, `pool.ts`, `orchestrator.ts`, `index.ts`, `drafts.ts`, `api.ts`, `assistant.ts`) imports from `engines/index.ts`:

- `engine.ts`: the `Engine` interface, `SessionSpec`, `SessionResult` and `describeFailure`.
- `claude.ts`: the Claude Code engine (`claude`), with `resolveClaude`, `sandboxSupported`, `buildArgs` and `sessionEnv`.
- `runner.ts`: `engines` (by id), `engineOf(id)` (missing means Claude Code), `Slots` and the engine-neutral `SessionRunner`.
- `transcripts.ts`: `startedOn` and `readTranscripts`.

- The runner stays one class and is engine-neutral: spawning a process, teeing its lines to the run's `.jsonl`, the watchdog, killing, slots and hooks don't change. What differs per engine moves behind an `Engine` interface in `agent/src/engines/`:

  | Member | Claude Code (`engines/claude.ts`) | Cursor (`engines/cursor.ts`) |
  |---|---|---|
  | `id` | `"claude"` | `"cursor"` |
  | `binary()` | today's `resolveClaude()` (`claude.exe` behind the npm shim, `RETASKABLE_CLAUDE`) | `cursor-agent` on `PATH`, or `RETASKABLE_CURSOR` |
  | `args(spec)` | today's `buildArgs` | section 4 |
  | `env(spec)` | today's `sessionEnv` (the local model) | `CLAUDE_CONFIG_DIR` for the lane isolate (section 4) |
  | `prompt(spec)` | the rendered prompt as is | the prompt plus the result instructions (section 5) |
  | `prepare(spec)` | nothing | writes the lane's allowlist and the isolate config dirs (sections 4 and 6) |
  | `reader(spec, id)` | one per session: `line(ev)` gives `rate_limit_event` → usage, `summarize` → events, `result` → the result from `structured_output`; `noResult(code, stderr)` when it exits without one | section 8, with the result extracted from the final message (section 5) |
  | `summarize(ev)` | `summarize.ts`, for reading a run back | section 8 |
  | `can` | `{ schema: true, cost: true, usage: true, budgetCap: true, sandbox: true, local: true }` | all `false` (`usage` is a 5-hour window reading, which Cursor never gives; its tokens are on `SessionResult.tokens`, section 7) |

- `SessionSpec` has `engine?: EngineId` (missing means `"claude"`; `EngineId` in `shared/types.ts` is `"claude" | "cursor" | "pi"`, pi being the third engine of section 13). `Session` and the run's `retaskable_meta` line record it beside the model, so a transcript read back later is parsed by the engine that wrote it (`readTranscripts`) and a resume knows where it started (`startedOn` returns `{ engine, model }`).
- In this Rust agent, the TypeScript above is the ground truth and `src/engines/` (`src/engines/mod.rs`) is the port: `src/engines/engine.rs` carries the `Engine`/`EngineReader` traits, `EngineId`, `SessionSpec`, `SessionStart`, `SessionResult` and `describe_failure`; `src/engines/claude.rs` is `ClaudeEngine` (its reader); `src/engines/resolve.rs` finds each engine's binary (`resolve_claude`, `resolve_cursor`, `resolve_pi`, `resolve_codex`); `src/engines/cursor/` is `CursorEngine`, split into `mod.rs` (the `Engine` dispatch), `allowlist.rs` (the lane allowlist and isolate config `prepare` writes, sections 4 and 6), `reader.rs` (`EngineReader`) and `result.rs` (pulling the structured result from the final message, section 5) to stay under this repo's 1000-line-per-file cap; `src/engines/pi.rs` is `PiEngine` (its reader, pulling the structured result from the final message the same way as Cursor's, section 13); `src/engines/json_result.rs` holds `extract_json`/`check_schema`, shared between Cursor's and pi's readers since both pull their result from the final message's own fenced block; `src/engines/runner.rs` is `engine_of` and the engine-neutral `run_session`/`spawn_and_read`, plus `TranscriptSink` for teeing a session's raw lines to its `.jsonl`; `src/engines/transcripts.rs` is `started_on`/`read_transcripts`, and `src/engines/summarize.rs` (plus each engine's own `summarize`) is the per-engine side of reading a stored line back into a `SessionEvent` (section 8). There is no `Slots` here: concurrency is the lane pool instead (docs/specs/agent/04-architecture.md section 4.1, `src/lanes.rs`), one session per lane.
- `run_session` writes a run's `.jsonl` through an optional `TranscriptSink` in exactly the format `read_transcripts` reads back (`cargo test` round-trips a session through both), but nothing in this crate calls it with a real project's own lane and run path yet, and there is no resume support on top of it: that's the pool/orchestrator that owns a session's lifecycle, still a later piece.
