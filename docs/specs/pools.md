# Pools: run what the server sends — Spec

Status: v0.3, built in part (2026-11-17). Backlog: `pool-tasks`. Copies the TypeScript agent's pool work (`poolTasks.ts`, `poolReport.ts`, `fixTasks.ts`, `jobTasks.ts`), which Retaskable.com's [pools-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pools-on-server.md) and chores-on-server.md stay the source for. Builds on [agent.md](agent.md), [pieces.md](pieces.md) and [agent/07.5-what-a-claim-and-a-give-back-send.md](agent/07.5-what-a-claim-and-a-give-back-send.md).

Theme: Orchestration

Kind: change

## 1. What this is

The store stops answering `claim_next`, `lane_take` and `agent_settings` once the agent release ships. Work comes as **tasks**: the computer reports `health` with `obeys: true` and the answer lists what to run. A task is an item's whole chain on this computer. Pieces still come by `run_take` (pieces.md), side by side in the TypeScript agent; here runs mode keeps `run_take` only.

## 2. Decided

- The computer starts nothing the store did not send; with the store unreachable it starts nothing (fail closed, no fallback).
- A finishing write (`task_end`) is kept in the state file until the store answers, also across a restart (the owner's outage rule: finish what it is doing, take nothing new).
- The tier is the store's (`smart` or `fast`); the model is the computer's: `smart` is the item flow's `opus` tier, `fast` is `sonnet` (`Tier::item_tier`).

## 3. As built (claim mode)

- **Ops** (`src/backlog/pool.rs`, tests with a copy of the store's argument rules in `pool/contract.rs`): `health`, `task_end` (answer `{ held }`; `false` drops the end), `claim { id, agent }` by name (the store answers its own lease token), `chore_trigger` (`red_main`, `playtest_fix`, `staging_smoke`, `behind_main`, fields cut to the store's limits) and `chore_nofix`, `chores_get` (`noFixSha`).
- **Report** (`src/orchestrator/driver/pool.rs`): once a minute per project, at once on `work_available`, paused or not: `{ obeys, can: [], project, agent (machine, environment, baseBranch, tags, week), running: [{ id }], usage (the open five-hour window), limits (engine to epoch ms), spend, refused?, paused, draining }`. `can` is empty (no job is run here), so the store sends no job, triage or release-notes task. `refused` is `paused after an own failure`, `signed out` when no lane on another engine can start, or the port guard's words.
- **Receiving** (`src/pool_tasks.rs`): a task id is taken once per run of the agent (a restart reports the tasks it still runs, kept as `state.pool_tasks`, so they are not taken again). Nothing is received while paused (own or store pause, or the store's unknown), stopping, with no session slot, no lane that may start, or a full model cap.
  - an item (`subject` is the item's id): claimed by name, `take_lease`, then the existing flow starts it. `conflict`, `not_found` and `held_by_owner` end the task not ok; any other error forgets it, and the store sends it again;
  - `chore:specs`: `draft_queue` with the task's words and `chore: "specs"`, and the task ends ok at once;
  - every other chore (`fix_main`, `fix_playtest`, `fix_smoke`, `sync_main`, `tidy`, `tests`, `bugs`) and every job: ended at once, not ok, with `This agent doesn't run <kind> tasks yet.` or `This computer doesn't run <kind> jobs.` They have no vehicle here yet (section 4).
- **Ending**: the task is open while its item is queued, implementing, checking or reviewing; ok once the item is handed on (awaiting CI, merging, awaiting merge, awaiting playtest, merged, done); not ok when it needs clarification or a person, or has left the loop. The end is written to `state.task_ends_owed` before it is sent, sent again every minute until answered; a coded refusal drops it; one older than a day is dropped.
- **`stop`** in the answer is logged only (the TypeScript agent ignores it too).

## 3a. The item flow as built (claim mode)

A step is a future the loop runs as the item's own task (`steps::Running`); it reads the lane and the code host, never the store, and hands back the run, a flow record, notes for the item's history and, when the item goes back to its implementer, a message. The loop (`driver/flow.rs`) applies that, queues the resume, and tells the store. State transitions stay `item_lifecycle`'s plain code.

- **`checking`** (`flow/checks.rs`): the lane's processes are stopped, the project's checks run with the baseline comparison (a failure main has too is `preexisting`), the evidence (marks, diff stat) is kept for the review. A blocking failure goes back to the implementer's session, resumed with `The project's checks failed on your branch:` and each failing check's output tail, up to `maxCheckRounds`, then a person; a pass goes to `reviewing`. The change the session opened is found by its branch (`gh pr list --head`) and kept.
- **`reviewing`** (`flow/review.rs`): an approval that still covers the head is reused (same head, a rebase whose own changes read the same, a conflict's resolution of at most 12 lines: `piece_approval::reuse`, the same rule a piece's run applies). Otherwise the scouts and the reviewer read the lane (`piece_review::run_lane_review`, shared with the runs) and the comment goes on the change. Approved: the head is the approved one; a diff that raises the tier (`piece_tier::held`) or a try-out the project's `playtests` asks for goes to `awaiting_playtest` (the held reasons in the try-out's words), `after_merge` keeps the try-out for after the merge (`flow.pendingPlaytest`), else to the merge (or CI with `ciWhenReady`). Requested changes go back (`The reviewer requested changes (round N):` ...) up to `maxReviewRounds`; a person's steps or a decision go to `needs_human` with the TypeScript reasons. A usage limit leaves the item in `reviewing` and the step is asked again later (`flow.retryAfterMs`).
- **Sending back**: the message is kept in the item's record (`flow.followUp`) and the session is resumed with it (`Launch::Resume`, `StartRequest.follow_up` with `resume`); one that cannot be resumed (grown, another engine) gets the message in a fresh session's prompt. The record (`RunRecord.flow`: change, rounds, approved head, last review, try-out) is in the state file, so a restart carries on mid-chain.
- **What the store hears**: `report { id, token, status, detail, branch, prUrl, note?, claudeOnIt? }` whenever an item's status, state, branch or change moved (`claimed`, `building`, `in_review`, `merging`, `needs_person`), kept and sent again until answered (`backlog/progress.rs`, TypeScript `HostedWork.changed`, `hostedWork.ts:603`). A refusal is a `store_error`; `stale_lease` ends it.
- **A chore item** (local, no store item) takes the same steps: a step takes no store, and the loop leaves `report`, `park`, `complete` out for an item it holds no lease for (`progress_of` returns nothing without one); the PR is found by branch.

## 4. Not built

- **The claim-mode item flow stops after the review (partly built, `item-flow` backlog).** The loop runs the steps of a leased item in `src/orchestrator/flow/` (`ClaudeStarter::advance`, which was `None`): `checking` (`flow/checks.rs`, TypeScript `checkStep`, `stepsCi.ts:260`) and `reviewing` (`flow/review.rs`, `review.ts:32`) are built, see section 3a. Still not built, so an item approved by the review waits in `merging` (or `awaiting_ci` with `requireCi`) with its lane freed: the PR and CI watch (`pollCi`, `stepsCi.ts:119`, `rerunCi`, `ciWatchFix.ts`), the merge (`mergeStep`, `merge.ts:128`, the store's merge turn in `mergeQueue.ts`, `awaitMergeStep`, `landed` and the `complete` after it), try-outs (the `playtest` details field, `storeTryouts.ts`) and reopen (`reopen.ts`). Until they are, nothing here carries an item to a merge, and only pieces (`run_take` runs: build, watch, merge, look) run a chain to a merge in this agent. This still decides how chore tasks can run (open question in the report that came with this spec).

- Chore tasks: a chore needs an item with no record in the store, through the item flow (worktree, session from the task's prompt, PR, checks, merge, no store writes for it), in both modes. Runs mode has no item flow, so it also needs one beside its runs.
- Jobs (`task.job`: lighthouse, probes, a11y, vet, deprecations, triage, release notes).
- `chore_trigger` senders (`red_main`, `staging_smoke`, `playtest_fix`, `behind_main`): not sent until the task they cause can run, since a waiting `red_main` holds back plan drafts in the store.
- Per-session cost in `running`, the `pool_compare` event.

## 5. Tests

`src/backlog/pool/tests.rs`, `src/pool_tasks/tests.rs`, `src/orchestrator/driver/tests/pool.rs`, and for the item flow `src/orchestrator/flow/{checks,review}_tests.rs` (real repo, origin, lane and fake `gh` and `claude`) and `src/orchestrator/driver/tests/flow.rs` (send-back resumed, kept across a restart, the store's reports, a step asked to wait): the report once a minute and on a wake, an item claimed by name with the store's tier, a refusal ending the task, a transient failure retried, an unreachable store starting nothing, chores and jobs ended in words, the specs chore, the end when the chain is handed on or stops, an end kept across a restart and sent when the store is back, a restart reporting the tasks it runs.
