# Agent telemetry and watchdog — Spec

Status: v0.1. From the human on 2026-10-15: "add more meta data and tracking so we know why and when things get stuck or the agent does something stupid". Section 2's **[Decided]** lines are what the human asked for; the **[Proposal]** lines fill the gaps and are the human's call. Builds on the orchestrator loop ([agent.md](agent.md) section 7), the restart ([restart-progress.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/restart-progress.md)) and claiming from the shared backlog ([claim-while-working.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/claim-while-working.md)).

Theme: Orchestration

Kind: new

## 1. What it is

Every problem on 2026-10-15 was found by hand, digging through item timelines, a live log without timestamps, transcripts in `runs/` and `state.json`:

- items handed back by the store carried on at a stale "needs a person" stop, holding their lease 8 hours with nothing running;
- one claimed item started in three lanes in one tick;
- after most merges the agent waited to restart with no new sessions starting, so lanes sat idle (0.86 of 4 busy on average);
- a PR's number was lost and every `gh` call for an hour went to PR "undefined" until 5 retries stopped it;
- a review round was logged twice because a restart cut off a review waiting for its slot;
- details writes failed again and again with "The lease on X has moved on".

This adds three things to the local agent:

1. **An event log** (section 3): what the agent did and when, one line per event, per project.
2. **A watchdog** (section 4): plain-code rules, checked every minute, that say when something looks wrong (an **anomaly**), reported in the snapshot the site reads.
3. **Two commands** (section 5) that read the log back: `npm run why -- <item>` and `npm run agent-report`. A third, `npm run start-report`, reads the session transcripts for how implementers get going.

Nothing here decides anything: the watchdog only reports. The orchestrator's control flow is unchanged.

## 2. Decisions

- **[Decided]** Per project, the agent keeps an append-only event log of its own doings: item state changes, lanes, sessions, checks, reviews, CI, merges, store failures, leases, restarts and a summary every minute.
- **[Decided]** A watchdog runs each minute from the pool's tick and records an anomaly when an invariant breaks, with an event when it starts and when it ends (never one per minute), and a line in the live log when it starts.
- **[Decided]** Anomalies go in the snapshot the site reads (`State.orchestrator.anomalies`). How the Expo app shows them is the app's.
- **[Decided]** `npm run why` and `npm run agent-report` read the event files only.
- **[Decided]** No transcript text, prompts or secrets in the log ([agent.md](agent.md) section 2; transcripts stay in `runs/` and are never sent to the cloud).
- **[Proposal]** The log lives with the project's state, `.retaskable/projects/<id>/events/`, one file per UTC day, kept 14 days.
- **[Proposal]** The watchdog's thresholds are named constants in `agent/src/telemetry/watchdog.ts`, not settings.

## 3. The event log

`agent/src/telemetry/events.ts` (`EventLog`); in this Rust agent, `src/telemetry/events.rs` (the log) and `src/telemetry/event.rs` (the events as types: an `Event` enum with one variant per kind in the table below, wrapped in an `Entry` with its time and project; a line of a kind it does not know, or whose fields do not fit its kind, is kept whole as `Body::Other` and still reads, and fields a variant does not name are written back as they came).

### 3.1 Files

- `.retaskable/projects/<id>/events/<YYYY-MM-DD>.jsonl`, the day in UTC. One JSON object per line, appended (never rewritten): `{ "at": ISO time, "kind": string, "item"?: id, "lane"?: number, ...fields }`.
- Files older than 14 days (`KEEP_DAYS`) are deleted when the log opens (the project is configured) and once a day after.
- The log is opened when the project is configured with a real id; until then (and in tests that never configure one) events are kept in memory only.
- On open it reads back the last 24 hours, so the watchdog's counts (repeated errors, steps left open by a restart) carry across a restart.
- A write that fails is dropped silently: the log must never stop the agent.

### 3.2 What goes in, and what never does

- Ids, states, lane numbers, durations (`ms`), counts, costs, model and engine names, session ids, PR numbers, store error codes.
- Short texts only: a timeline note or an error message is cut to 300 characters (`MAX_TEXT`), and anything that looks like a token or a secret (GitHub tokens `gh[pousr]_…` and `github_pat_…`, GitLab `glpat-…`, `sk-…` keys, Slack `xox[abprs]-…`, `Bearer`/`Basic …`, `token=`/`password=`/`secret=`/`api_key=`/`authorization:` values) is replaced with `[redacted]` first.
- Never a prompt, a transcript line, a follow-up message, a review's text or a session's output. Fields named `prompt`, `followUp`, `transcript`, `message`, `body`, `token` or `lease` are dropped whatever they hold.

### 3.3 Events

| kind | when | fields |
| --- | --- | --- |
| `item_state` | any item's state changes. Recorded in one place, `Store.update`, which compares the state before and after | `item`, `from`, `to`, `note` (the timeline note that came with it), `lane` |
| `lane_taken`, `lane_freed` | `laneItems` changes (compared on each save of the state, and each minute) | `lane`, `item`, `why` (the item's state, and its last note) |
| `lease` | an item's lease in the shared backlog changes (also in `Store.update`) | `item`, `change`: `taken`, `parked`, `released` or `lost` |
| `claim` | a `claim_next` to the store is answered | `answer`: the item id, `none`, or `failed` |
| `store_error` | any operation on the backlog store fails (`HostedBacklog.op`) | `op`, `item` (its `id` argument), `code`, `error` |
| `session_start` | a session starts | `item`, `lane`, `role`, `scout`, `engine`, `model`, `session`, `detached`, `attached` (picked up again after a restart) |
| `session_end` | a session ends | the same, plus `ms`, `cost`, `ok`, `subtype`, `error`, `limitHit` |
| `step_start`, `step_end` | the checks, a review round or a merge starts and ends | `item`, `step` (`checks`, `review`, `merge`), `key` (e.g. `slice 2 round 1`), `again` when the same step was already open; at the end `ok`, `ms`, `outcome`, and for checks each check's `name`, `ok` and seconds, for a review its `lines` and `scouts` |
| `ci` | the CI status read for an item changes | `item`, `state` (`pending`, `pass`, `fail`, `none`), `ms` since its wait began |
| `merged` | a pull request merged | `item`, `pr` |
| `error` | an item's step failed or a `gh` call for it was retried | `item`, `where`, `error` |
| `host_call_undefined` | a `gh` or `glab` call has `undefined` or `NaN` as an argument, or as part of a path or value in one (a programming error) | `cmd`, `args` (cut) |
| `restart_asked`, `restart_waiting`, `restart_exit`, `restart_back` | A restart: asked; each minute while it finishes a merge first (2 minutes at most since 2026-10-19); exiting; back | `reason`, `by`, `waitingOn` (each minute), `drainMs` (exit), `downMs` (back) |
| `tick` | every minute | `lanes`, `busy` (the busy lanes' numbers), `free`, `sessions`, `states` (items by state), `claim` (asked since the last tick, and the answer), `status` |
| `anomaly` | a watchdog rule starts or stops firing | `phase` (`start`, `end`), `id`, `rule`, `item`, `text`, `details`, and at the end `ms` |

Restart events are the machine's, so they go into every project's log.

**Status in the Rust agent:** the log itself, and opening it at startup, are built (`src/telemetry/events.rs`, `src/runtime.rs`). Written today: `tick`, `item_state`, `lane_taken`, `lane_freed`, `lease`, `claim`, `store_error`, `session_start`, `session_end`, `step_start` and `error` (`src/orchestrator/driver.rs` and its `settle`/`recovery` submodules, `src/orchestrator.rs`), plus `anomaly` (section 4). Not yet written anywhere: `step_end`, `ci`, `merged` (no CI-polling or merge code exists in this agent yet), `host_call_undefined` and the `restart_*` family (this agent has no restart state machine yet — see section 4's status note). Each lands with the backlog item or piece that does the thing it records, the same way `events.rs` was built before anything used it ([agent/11-build-milestones.md](agent/11-build-milestones.md)).

## 4. The watchdog

`agent/src/telemetry/watchdog.ts` (pure rules) and `agent/src/telemetry/telemetry.ts` (each project's state for them). Every minute the pool hands each project's `Telemetry.minute` a snapshot (`watchInput` in `agent/src/telemetry/watchInput.ts`) of what is in memory: its items, `laneItems`, what is running for each item (a step, a session, a start, a game in its lane), whether new work may start, whether there is work ready to start, the restart, and the log's recent events. No other I/O.

Each rule names an anomaly `{ id, kind, item?, since, text, details }`. It starts when a rule first finds it (after its threshold), and ends when a minute's check no longer finds it. A condition a rule needs to hold for a while is timed from the first check that saw it, so the clock starts again after a restart.

| rule | fires when | threshold |
| --- | --- | --- |
| `lease_idle` | this agent holds a lease on an item that has nothing running and isn't in a state the agent actively waits in (`awaiting_ci`, `awaiting_merge`, `merging`, `checking`), nor waiting for the usage limit or a free lane; or it holds the lease in a state it should have let go of while it waits (`parkFor`) | `LEASE_IDLE_MS` 10 min; `LEASE_PARK_MS` 5 min |
| `double_lane` | an item is in more than one lane, or an item's `lane` and `laneItems` disagree | at once for two lanes; `LANE_MISMATCH_MS` 2 min for a disagreement (a start fills `laneItems` first) |
| `idle_lanes` | lanes are free while new work may start (a session slot is free too) and there is work ready to start here, or claims aren't being asked of the store; also while a restart is pending (it says what the restart finishes first) | `IDLE_LANES_MS` 5 min |
| `repeat_error` | the same error (numbers, hashes and ids stripped) for the same item 3 or more times in 30 min; or any `host_call_undefined` in the last 30 min, at once | `REPEAT_ERRORS` 3 in `REPEAT_WINDOW_MS` 30 min |
| `no_progress` | an item in `implementing`, `checking`, `reviewing` or `merging` with nothing running for it, not waiting for the limit, a free lane or its turn to merge, and nothing in its timeline | `NO_PROGRESS_MS` 20 min |
| `restart_wait` | a restart has been pending | `RESTART_WAIT_MS` 15 min |
| `sync_failing` | writes of details, drafts, settings or chat to the store (`details_*`, `draft_*`, `settings_*`, `ask_*` writes) failing | 3 in `SYNC_WINDOW_MS` 10 min |
| `duplicate_step` | the same step (`step_start` with the same item, step and key) started again without a `step_end` in between, e.g. "Review round 1" twice | shown for `DUPLICATE_SHOWN_MS` 30 min after |

The `lease_idle`, `idle_lanes` and `no_progress` thresholds above are defaults: a project can set `leaseIdleMinutes`, `idleLanesMinutes` and `noProgressMinutes` in its `tuning` setting from retaskable.com ([project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md) section 3.1), with no new agent. Telemetry stays on the computer; nothing is rolled up in the cloud.

Nothing fires while the project has no config. `idle_lanes` and `no_progress` don't fire while the project is paused, over its budget or waiting for the usage limit (new work can't start then).

When an anomaly starts the agent prints one line to its console, e.g. `[watchdog 14:05] retaskable: R-12 has held its lease for 12m with nothing running (needs_human)`.

**Status in the Rust agent:** `src/telemetry/watchdog.rs` (the pure rules, called `check`) and `src/telemetry/debounce.rs` (each project's `Telemetry`, timing a finding from when it first holds and writing its `start`/`end` `anomaly` events), wired into `ProjectLoop::record`'s once-a-minute tick (`src/orchestrator/driver.rs`). Five rules are ported — `lease_idle`, `double_lane`, `idle_lanes`, `repeat_error`, `no_progress` — with these differences from the table above, each because the signal the TS rule reads doesn't exist yet in this agent:

- `restart_wait`, `sync_failing` and `duplicate_step` aren't ported. This agent has no restart state machine (no code ever builds a `RestartAsked`/`RestartWaiting`/`RestartExit`/`RestartBack` event), no write path for details, drafts, settings or chat to the store (`sync_failing`'s whole premise), and `step_start`'s `key` and `again` fields, while defined on the type, are never set by `advance()` — so there's no way yet to tell a step started twice from one still open.
- `lease_idle` only fires for a lease held at `needs_human`; it leaves out `implementing` (the TS rule's session-stale case), since that is already this agent's separate, shipped `tuning.sessionQuietMinutes` watchdog, entirely inside `run_session` (`src/orchestrator/session.rs`) and not surfaced to `ProjectLoop` per item — folding it in here would risk a false positive on a session that is in fact still running. `LEASE_PARK_MS`'s sub-case (a lease held in a state it should have let go of while it waits) isn't ported; there's no `parkFor` equivalent to read yet.
- `no_progress` is scoped to `checking`, `reviewing` and `merging` for the same reason: `implementing` is left to `sessionQuietMinutes`.
- `repeat_error`'s "numbers, hashes and ids stripped" is approximated by stripping runs of decimal digits only; a non-numeric hash or id (e.g. a git SHA) does not normalize away.
- `idle_lanes` and `no_progress` only skip while the project is paused; this agent has no budget or usage-limit signal threaded to the watchdog yet, so those two exclusions from the table above aren't checked.
- The three `tuning` overrides (`leaseIdleMinutes`, `idleLanesMinutes`, `noProgressMinutes`) are read from `TuningConfig` in `src/config/project.rs`, the same as `sessionQuietMinutes`.
- Anomalies are logged and printed to the console; sending them on to the snapshot the site reads (section 4.1) is the separate `snapshot-reporting` item's job, not this one's.

### 4.1 On the screens

- `State.orchestrator.anomalies`: the open anomalies, oldest first (`Anomaly` in `shared/types.ts`). The agent only sends them; the Expo app words and places them (the Agents screen's "What looks wrong" list, a warning line on an item's page).

### 4.2 Cursor's tokens on the Usage card

Cursor reports no limit or amount left, only each session's tokens, so the computer keeps a total of its own and reports it as a derived result (never a transcript) beside Claude Code's meters, in the snapshot's `usage.cursor` (`CursorUse` in `shared/types.ts`; `MachineStore.cursorUse`, `agent/src/store.ts`):

- `todayTokens`: input plus output tokens of the Cursor sessions that ended today, the same local day as the daily spend (`today()`), every project on the computer together.
- `weekInputTokens` and `weekOutputTokens`: the same over a **rolling 7 local days**, today included. Claude Code's weekly window comes from its `rate_limit_event` and has a reset time; Cursor gives none, so there is no window to follow.
- `limitHitAt`: when a Cursor session last hit Cursor's usage limit (ISO), kept until the next hit.
- Missing until a Cursor session has reported tokens; the card then reads "Usage not reported".
- Cache read and write counts aren't added, and none of it touches Claude Code's meters, cost or auto concurrency.

## 5. Reading it back

The first two commands read `.retaskable/projects/*/events/*.jsonl` only (`agent/src/telemetry/report.ts`, run by `scripts/why.ts` and `scripts/agent-report.ts`). `--project <id>` keeps to one project.

- `npm run why -- <item id> [--since <time>]`: that item's events in order, one line each with its time, with a line wherever nothing happened for 10 min or more ("… nothing for 8h 01m"); then its anomalies (when each started and ended), then its sessions (role, model, how long, cost, how it ended). `--since` takes an ISO time or a span back from now (`90m`, `6h`, `2d`); by default everything the log keeps.
- `npm run agent-report -- [--hours 24]`: for the window, per project:
  - **Lanes**: each lane's busy share and the overall average from the minute summaries, and the stretches of 15 min or more with every lane idle.
  - **Phases**: the median time an item spends in each state per slice (implementing, checking, reviewing, awaiting CI, merging, awaiting merge), and the rework rounds (back to implementing from checks, CI, review or a try-out), counted by why (the timeline note).
  - **Restarts**: each one, how long it drained before exiting and how long the agent was down.
  - **Anomalies**: each one in the window, its rule, item, text and how long it lasted.
- `npm run start-report -- [--sessions 20] [--project <id>]`: how the newest fresh implementer sessions got going (agent.md section 6.2's Where to start). One line per session with its turns, calls and share of the cost before its first edit, then the medians and the share of those turns that made a single call. It reads the session transcripts (`.retaskable/projects/*/runs/<item>/*.jsonl`), not the event log, since only they keep each turn's tool calls and usage (`agent/src/telemetry/startStats.ts`, run by `scripts/start-report.ts`). Chores and the agent's own runs are left out. A turn's cost weighs its usage by price: cache writes 1.25×, cache reads 0.1×, output 5×.

**In the Rust agent:** the three are `retaskable why -- <item id>`, `retaskable agent-report` and `retaskable start-report` (`src/telemetry/report/`, `src/telemetry/start_stats.rs`, `src/main.rs`), same flags and output. Like a release build's server, they read its own data folder (installers/03-the-bundle-agent-bundle.md section 3.3) unless `RETASKABLE_DATA_DIR` says otherwise. They only read what's on disk, so they stay accurate ahead of every `Event` being written (section 3.3): empty until the events that feed them exist.

## 6. Tests

- `agent/src/telemetry/events.test.ts` (and `src/telemetry/events.rs` here): the line format, one file per UTC day, pruning past 14 days, secrets redacted and long texts cut, dropped fields, reading back a window, and every kind round-tripping through `Event` (`src/telemetry/events.rs`, `event.rs`) with an unknown kind still reading.
- `agent/src/telemetry/watchdog.test.ts` (and `src/telemetry/watchdog/tests.rs` here, for the five ported rules): each rule fires, doesn't fire, and ends.
- `agent/src/telemetry/telemetry.test.ts` (and `src/telemetry/debounce/tests.rs` here): a finding below threshold writes nothing, crossing it writes one `start`, clearing writes one `end` with how long it held, and a zero-threshold finding starts on its first tick.
- `agent/src/telemetry/report.test.ts` (and `src/telemetry/report/tests.rs` here): both readers on a fixture log.
- `agent/src/telemetry/startStats.test.ts` (and `src/telemetry/start_stats.rs` here): a transcript's numbers before the first edit, which sessions count, the report's medians.
