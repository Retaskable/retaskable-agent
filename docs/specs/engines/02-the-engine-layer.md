## 2. The engine layer

Built (slice 1). What was `agent/src/claude.ts` is now `agent/src/engines/`, and every importer (`steps.ts`, `setup.ts`, `releaseNotes.ts`, `playtestGroups.ts`, `pool.ts`, `orchestrator.ts`, `index.ts`, `drafts.ts`, `api.ts`, `assistant.ts`) imports from `engines/index.ts`:

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

- `SessionSpec` has `engine?: EngineId` (missing means `"claude"`; `EngineId` in `shared/types.ts` is `"claude" | "cursor"`). `Session` and the run's `retaskable_meta` line record it beside the model, so a transcript read back later is parsed by the engine that wrote it (`readTranscripts`) and a resume knows where it started (`startedOn` returns `{ engine, model }`).
- **Slice 1 changes no behaviour.** The Claude Code engine is the old code moved, and every existing test passes unchanged (their imports point at `engines/index.ts`); `engines/claude.test.ts` tests the Claude Code reader. No config, route or UI change; the only State change is `engine: "claude"` on each session.
