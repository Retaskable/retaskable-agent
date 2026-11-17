# Pools: run what the server sends — Spec

Status: v0.2, built in part (2026-11-17). Backlog: `pool-tasks`. Copies the TypeScript agent's pool work (`poolTasks.ts`, `poolReport.ts`, `fixTasks.ts`, `jobTasks.ts`), which Retaskable.com's [pools-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pools-on-server.md) and chores-on-server.md stay the source for. Builds on [agent.md](agent.md), [pieces.md](pieces.md) and [agent/07.5-what-a-claim-and-a-give-back-send.md](agent/07.5-what-a-claim-and-a-give-back-send.md).

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

## 4. Not built

- **The claim-mode item flow stops at `checking`.** `ClaudeStarter::advance` (`src/orchestrator/session.rs:260`) is `None`: checks, CI, review and merge steps are not built for leased items. A task's item therefore ends its task ok only by being handed on, and nothing here carries it to a merge. Only pieces (`run_take` runs: build, watch, merge, look) run a chain to a merge in this agent. This decides how chore tasks can run (open question in the report that came with this spec).

- Chore tasks: a chore needs an item with no record in the store, through the item flow (worktree, session from the task's prompt, PR, checks, merge, no store writes for it), in both modes. Runs mode has no item flow, so it also needs one beside its runs.
- Jobs (`task.job`: lighthouse, probes, a11y, vet, deprecations, triage, release notes).
- `chore_trigger` senders (`red_main`, `staging_smoke`, `playtest_fix`, `behind_main`): not sent until the task they cause can run, since a waiting `red_main` holds back plan drafts in the store.
- Per-session cost in `running`, the `pool_compare` event.

## 5. Tests

`src/backlog/pool/tests.rs`, `src/pool_tasks/tests.rs` and `src/orchestrator/driver/tests/pool.rs`: the report once a minute and on a wake, an item claimed by name with the store's tier, a refusal ending the task, a transient failure retried, an unreachable store starting nothing, chores and jobs ended in words, the specs chore, the end when the chain is handed on or stops, an end kept across a restart and sent when the store is back, a restart reporting the tasks it runs.
