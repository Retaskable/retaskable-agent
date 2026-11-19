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
| `item_state` | any item's state changes. Recorded in one place, `Store.update`, which compares the state before and after | `item`, `from`, `to`, `note` (the timeline note that came with it), `lane`, `claudeOnIt` (on an item at `needs_human`: whether Claude is looking into it or about to, 06.9) |
| `lane_taken`, `lane_freed` | `laneItems` changes (compared on each save of the state, and each minute) | `lane`, `item`, `why` (the item's state, and its last note) |
| `lease` | an item's lease in the shared backlog changes (also in `Store.update`) | `item`, `change`: `taken`, `parked`, `released` or `lost` |
| `claim` | a `run_take`, a `merge_next` for a piece's merge, a `merge_open` that resumes a held turn (or the old `claim_next`) to the store is answered | `op`; `answer`: the item id, `none`, `passed`, `waiting_for_needs`, or `failed` |
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

**Status in the Rust agent:** the log itself, and opening it at startup, are built (`src/telemetry/events.rs`, `src/runtime.rs`). Written today: `tick`, `item_state`, `lane_taken`, `lane_freed`, `lease`, `claim`, `store_error` (the loop's own reads, and every failed `draft_report`, `draft_take`, `draft_release`, `draft_answer_taken`, `draft_queue` and `ask_report`, with `item` the draft or ask id; the spawned draft and ask tasks keep theirs in `backlog::failures::Failures` and the loop writes them on its next pass. `draft_front` has no caller yet), `session_start`, `session_end`, `step_start`, `step_end` and `error` (`src/orchestrator/driver.rs` and its `project`/`settle` submodules, `src/orchestrator.rs`), plus `anomaly` (section 4). Also written now: `restart_asked`, `restart_waiting` and `restart_exit` (`src/restart.rs`'s state machine, logged by `src/runtime.rs`'s restart sequence). Not yet written anywhere: `restart_back` (the type and its report exist; nothing logs it when the agent starts again). Also written: `ci` (`orchestrator/watch_run/mod.rs` sends it over the run-event channel, `RunRequest.events`, after each successful read whose state differs from the last one sent in that watch, `item` the piece id, `ms` since the watch began; `none` when there are no checks or the read ends Green with the no-CI note; `gh` errors send nothing; the loop writes it in `src/orchestrator/driver/runs.rs`). `merged` is written when a merge run ends `merged` (`item`, the PR number read from the piece's change, whether a number or the end of a PR URL, and `chore`), once per run; with no number to read nothing is written (`src/orchestrator/driver/runs.rs`). Also written: `host_call_undefined`, for calls through `ci_watch::gh_out` (`ci_watch::read_checks`, `orchestrator/merge_run.rs`) and for the `gh` calls in `implementer/publish.rs`, `implementer/abandon.rs`, `drafts/land.rs`'s `merge_pr` and the default-branch read in `orchestrator/driver/report.rs`, each checking only its identifying arguments (branch, PR, merge flag, repo), never a title, body or comment (`drafts::land::close_pr`, `piece_review.rs`'s comment and `driver::repo_id`'s `gh api` take a number or run with no project directory, so they are not guarded); and, in this slice, the rest: the agent CI's `prs::list`, automerge and `report.rs` (its `api` method and path only), release notes' `gather`, `repo.rs`, `repo_gitlab.rs` and `profile.rs`, and `driver::repo_id`'s `gh api`: `host_call::guard` (or `guard_program`, which records the program's file stem, `gh` or `glab`, never a path) refuses the call (the caller gets an error saying which argument was undefined) and keeps `cmd` and the cut `args` in a process-wide sink, and each pass of the loop (`src/orchestrator/driver/host_calls.rs`, draining the sink in `src/host_call.rs`) writes one event per refusal to the log of the project whose `repo_path` holds the call's directory, or of every project when none does. Each lands with the backlog item or piece that does the thing it records, the same way `events.rs` was built before anything used it ([agent/11-build-milestones.md](agent/11-build-milestones.md)).

**[Proposal] Slices for the unwritten events.** Each lands with the code that does the thing it records:
1. `step_end`'s checks and review detail: `step_start` carries `key` (`slice 2 round 1`, from the run's slice and its check or review round; `slice 2` for other steps) and `again` (the same item, step and key already open in the log with no `step_end`), and every step ends with a `step_end` (`ok`, `ms`, `outcome` = the state it left the item in; `ok` false and outcome `panicked` or `cut` for a step that panicked or was cut by a restart). A step hands back a `StepDone` (the item's run and a `StepDetail`: `checks` as `CheckTime { name, ok, sec }`, `lines`, `scouts`); `step_end` writes `checks` when the detail has any, each check's seconds being `CheckRun::seconds`, measured around the runner call. For a `reviewing` step whose detail has them (never on a `panicked`/`cut` end), `step_end` also writes `lines` (the diff's `total_lines`) and `scouts` (the length of `plan_scouts`' jobs; 0 when the reviewer reads a small diff alone), built by `StepDetail::review`; `describe` shows them as `(40 lines, 2 scouts)`. No step reports any detail yet (the Rust agent has no checks step, and `advance` returns `None`).
2. `ci` and `merged`: `ci` is written when the CI status read for an item changes (`item`, `state`, `ms` since its wait began), never twice for the same state; `merged` once when its pull request merges (`item`, `pr`).
3. `host_call_undefined`: every `gh` or `glab` call checks its arguments for an undefined or NaN-like value (an empty id, a `NaN` number) and, on one, writes the event with `cmd` and `args` (cut) and does not run the call.
Acceptance for each: the event type in `src/telemetry/events.rs` is written with every field above, a test drives the step and reads the event back from the log, and the "not yet written" list in the status note shrinks to what is left.

### 3.4 Measurement events (built here, in the Rust agent)

So a trial of a few days can be read per computer from the log alone: what every session cost, what got merged, what needed a person and why. Where the TypeScript agent has the kind, the field names are the same (`session_end`'s `cost`, `tokens`; `merged`'s `item`, `pr`, `chore`), so both agents' logs compare directly. The rest are **built here**: TS does not write them yet. Every text goes through section 3.2's scrub and 300-character cut; none holds a prompt, a transcript line or a token (a run is named by `run`, twelve hex digits of its token's hash, never the token).

| kind | when | fields |
| --- | --- | --- |
| `session_end` (more) | **Every engine session of every role** ends: implementer, reviewer, scouts, drafter, planner, assistant (an ask or chat), writer (release notes, ideas, a CI fix's summary), triager and a CI fix (`item` `_ci-<pr>`). Written by the project's loop on its next pass, from the runner's one chokepoint (`engines::run_session`, `orchestrator/live/session_events.rs`). A claim-mode implementer's engine sessions since its last settle are summed onto the one `session_end` its settle writes, as before | beside section 3.3's: `cost` (USD, the engine's own report, `total_cost_usd` for Claude as TS records it; left out when the engine reports none, as Cursor and Codex do), `tokens` (input plus output, as TS), `inputTokens`, `outputTokens`, `cacheTokens` (cache reads and writes, when reported), `piece` and `run` (runs mode only: the piece id and the run's name) |
| `merged` (more) | An item's slice merges (`driver/item_merge.rs`, once the store has the result), a merge run ends `merged` (`driver/runs.rs`) | beside `item`, `pr`, `chore`: `commit` (the merge commit, when read) and `how`: `merge` (the agent landed it), `auto_merge` (the host did once its checks passed, the project's `land`) or `away` (it merged while the agent was away from the item). A merge run cannot tell `away`; it says `merge` or `auto_merge` |
| `draft_landed` | A plan's pull request merged here, pressed by the agent or merged by a person on the host (`driver/land.rs`) | `draft`, `pr`, `items` (how many items the plan adds) |
| `chore_start`, `chore_end` | A chore began on this computer (a claim-mode chore item taken, or a runs-mode chore piece's first build run) and ended. A runs-mode chore ends at its merge run, or sooner when a run ends `nothing_left`, `failed`, or with questions | `item`, `chore` (`fix_main`, `tidy`, `tests`, ...), `mode` (`claim` or `runs`); at the end `outcome`: `merged`, `nothing_left`, `stopped` (parked for a person) or `failed`, and `ms` since it began |
| `run_start`, `run_end` | A runs-mode run starts, and ends (every outcome the loop hears of; a run cut by the store's `stop` that never reports gets no end) | `run`, `runKind` (`plan`, `build`, `fix`, `watch`, `merge`, `look`; not `kind`, which is the event's own), `piece`, `pr` (the pull request's number) and `url` (its URL, when the store gave one) on both events; at the end `outcome` (the store's word: `planned`, `pr_opened`, `merged`, `failed`, ...), `ms`, and for `failed` the `why`, cut |
| `needs_person` | An item (claim mode, in `moved` when it enters `needs_human`) or a run (runs mode, ending `failed`, `questions` or `person_steps`) needs a person | `item`, `reason` (a short code), `text` (the words, cut), `from` (the state, or the run kind), `run` (runs mode) |

**PR on run events.** `pr` and `url` are the piece's change as the store holds it (`change`, or the `prNumber` and `prUrl` of its piece record: a number, a URL, or a record with either). A fix, a watch and a merge run have it when they start, so both events carry it. A build has none at its start; its `run_end` carries the `prNumber` and `prUrl` of the change it opened (the `pr_opened` result, or a `questions` end that pushed a slice). An event with no change known leaves both fields out. Per-PR timing and attribution are then a sum over `pr`, not a match on times (a 2026-11-19 comparison with the TS agent had to match runs to PRs within 90 seconds). The `merged` event's `pr` is unchanged.

`reason` is one of `push_rejected`, `no_commit`, `missing_program`, `ci_red`, `checks_red`, `questions`, `person_steps`, `nothing_left`, `store_refused`, `review_escalated`, `merge_conflict`, `merge_failed`, `pr_closed`, `limit`, `gave_up`, `session_failed` or `other`, chosen from the text by `telemetry::measure::reason_code` (the first match wins; the text always rides along, so an `other` can be regrouped later).

Not covered: the Chat Agent is a separate program with no project log, and the design board's and classifier's one-shot haiku calls run with no project row, so neither writes a `session_end`.
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
| `duplicate_step` | the same step (`step_start` with the same item, step and key) started again without a `step_end` in between, within one run of the agent, e.g. "Review round 1" twice. A step the agent starts again because it restarted (or took the item back) isn't a duplicate: the earlier start belongs to the run before **[Proposal]** | shown for `DUPLICATE_SHOWN_MS` 30 min after |

The `lease_idle`, `idle_lanes` and `no_progress` thresholds above are defaults: a project can set `leaseIdleMinutes`, `idleLanesMinutes` and `noProgressMinutes` in its `tuning` setting from retaskable.com ([project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md) section 3.1), with no new agent. Telemetry stays on the computer; nothing is rolled up in the cloud.

Nothing fires while the project has no config. `idle_lanes` and `no_progress` don't fire while the project is paused, over its budget or waiting for the usage limit (new work can't start then).

When an anomaly starts the agent prints one line to its console, e.g. `[watchdog 14:05] retaskable: R-12 has held its lease for 12m with nothing running (needs_human)`.

**Status in the Rust agent:** `src/telemetry/watchdog.rs` (the pure rules, called `check`) and `src/telemetry/debounce.rs` (each project's `Telemetry`, timing a finding from when it first holds and writing its `start`/`end` `anomaly` events), wired into `ProjectLoop::record`'s once-a-minute tick (`src/orchestrator/driver/project.rs`). Seven rules are ported — `lease_idle`, `double_lane`, `idle_lanes`, `repeat_error`, `no_progress`, `restart_wait`, `duplicate_step`, `sync_failing` — with these differences from the table above, each because the signal the TS rule reads doesn't exist yet in this agent:

- `restart_wait` and `idle_lanes`' restart clause read `watchdog::Input::restart` (a `RestartView`: `Why::at` and `Step::Waiting`'s list, filled in from `restart::state()` by `ProjectLoop::check_watchdog`); `restart_wait` fires at once past `RESTART_WAIT_MS`. `duplicate_step` reads `Input::recent` only, fires at once and is shown for `DUPLICATE_SHOWN_MINUTES` after the latest repeated `step_start`; a start with no `key` never counts. **[Proposal]** Only `step_start`s written since this run of the agent began count (`Input` carries the run's start time, taken when the process starts; a `step_start` older than it, from before a restart, is neither open nor repeated), so a resumed step shows no `duplicate_step` while two starts in the one run still do. Tests in `src/telemetry/watchdog.rs`: a start before the run began and the same start after it raise nothing; two starts in the run with no `step_end` between raise one. `advance()` sets `step_start`'s `key` and `again` and writes `step_end`, so the rule has input to read.
- `lease_idle` only fires for a lease held at `needs_human`; it leaves out `implementing` (the TS rule's session-stale case), since that is already this agent's separate, shipped `tuning.sessionQuietMinutes` watchdog, entirely inside `run_session` (`src/orchestrator/session.rs`) and not surfaced to `ProjectLoop` per item — folding it in here would risk a false positive on a session that is in fact still running. `LEASE_PARK_MS`'s sub-case (a lease held in a state it should have let go of while it waits) isn't ported; there's no `parkFor` equivalent to read yet.
- `no_progress` is scoped to `checking`, `reviewing` and `merging` for the same reason: `implementing` is left to `sessionQuietMinutes`.
- `sync_failing` reads only `store_error` events in the recent log (`details_`, `draft_`, `settings_`, `ask_` ops), project-wide with no item and threshold 0; its details carry `count` and `ops`. Other ops don't count. The matching ops this agent writes today are `draft_claim_next` and the draft and ask writes listed in section 3's status note; nothing writes `details_` or `settings_` store errors yet.
- `repeat_error`'s "numbers, hashes and ids stripped" strips runs of decimal digits and hex hashes (7 or more hex characters with a digit in them, e.g. a git SHA); an id in other words still does not normalize away.
- `idle_lanes` and `no_progress` skip while the project is paused or `watchdog::Input::cant_start` is set by the same rules `claim` uses (`ProjectLoop::start_blocked`): a claim pause, or no lane whose engine, after fall-over, may start. This agent doesn't know a daily-budget stop, so that exclusion isn't checked.
- The three `tuning` overrides (`leaseIdleMinutes`, `idleLanesMinutes`, `noProgressMinutes`) are read from `TuningConfig` in `src/config/project.rs`, the same as `sessionQuietMinutes`.
- Anomalies are logged and printed to the console. Each minute's `ProjectLoop::check_watchdog` also copies `Telemetry::open_anomalies()` into the project's `ProjectLive.anomalies` (`src/orchestrator/live.rs`), which signals the live change when the list changes. `orchestrator.anomalies` in the snapshot (`src/view/orchestrator.rs`) is every project's list merged oldest first, each `{ id, kind, item?, since, text, details, project }`: `text` scrubbed and cut to 300 characters like the log, `details` cut to the first keys that write out within 1 KB, at most 20 with `anomaliesMore: n` for the rest; the keys are left out when there are none.

### 4.1 On the screens

- `State.orchestrator.anomalies`: the open anomalies, oldest first (`Anomaly` in `shared/types.ts`). The agent only sends them; the Expo app words and places them (the Agents screen's "What looks wrong" list, a warning line on an item's page).

What the `snapshot-reporting` item needs the computer to hold and send, all **[Proposal]**:

- **The record.** `Telemetry` (`src/telemetry/debounce.rs`) today keeps only when a finding was first seen and whether it has started. To hand a snapshot its anomalies it also keeps, for each started one, the fields of its `start` event: `id`, `rule`, `item`, `text`, `details`, and `since` (the time of the first check that saw it, not of the `start` event, so a 10-minute threshold shows `since` 10 minutes before the event). One read, `open_anomalies()`, returns them oldest `since` first, ties by `id`.
- **What is sent.** Each anomaly as `{ id, kind (the rule), item?, since (ISO), text, details }`, the shape section 4 names. `details` is the rule's own small object (counts, minutes, lane numbers, the item's state), never longer than 1 KB once written out; a larger one is cut to its first keys. `text` goes through the same redaction and 300-character cut as the log (section 3.2), since it also reaches the site.
- **Many at once.** At most 20 open anomalies are sent, oldest first, with `anomaliesMore: n` beside them for the rest, so one broken project cannot make the snapshot grow without end. The log still gets every `start` and `end`.
- **Several projects.** The computer sends all its projects' anomalies in one list; each carries its project id so the site can place it under its own project.
- **When one ends.** It is in the next snapshot's list until the minute whose check no longer finds it; then it is gone from the list at once (no "resolved" row). Its `end` event, with `ms`, is the only record, and `retaskable why` and `agent-report` read it there.
- **A restart.** Open anomalies are not saved. After a restart the list is empty, and a condition that still holds starts again after its full threshold, with a new `start` event and a new `since` (section 4's "clock starts again"). The log's `end` for the old one is never written, so a reader treats a `start` with no `end` before a `restart_back` as ended by the restart.
- **Paused and offline.** A paused project keeps its open anomalies on the list until their rules stop finding them (`idle_lanes` and `no_progress` end at once, since they do not fire while paused). With the cloud link down the snapshot is not sent, and the next one that is sent carries the current list, not what was missed.
- **Not sent.** Nothing from the event log's other kinds, and no anomaly that is still under its threshold.

Tested, once built: `open_anomalies()` is empty below the threshold, lists a started anomaly with `since` from its first check, drops it the minute it ends, orders by `since`, and caps at 20 with the count of the rest (`src/telemetry/debounce/tests.rs`).

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
