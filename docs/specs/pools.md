# Pools: run what the server sends — Spec

Status: v0.1, draft (2026-11-11). Backlog: `pool-tasks`. Copies the TypeScript agent's pool work (`poolTasks`, `poolReport`, `fixTasks` and `jobTasks`; Retaskable.com #3405, #3400, #3408, #3409, #3447, #3478 and #3190), which Retaskable.com's specs stay the source for. Builds on [agent.md](agent.md), [pieces.md](pieces.md) and [agent/07.1-model-tiers.md](agent/07.1-model-tiers.md). Everything the request did not spell out is marked **[Proposal]**; the TypeScript code decides where it differs, and the implementer stops and asks if it conflicts with a decided line.

Theme: Orchestration

Kind: new

## 1. What this is

Work comes from the server. The computer no longer decides what to start: it polls the project's pool for **tasks**, runs each one, and reports. A **task is an item's whole chain**: the build, its fixes, its checks, its review and its merge are one unit the server hands out, not separate asks. This replaces asking for runs or claims one at a time ([pieces.md](pieces.md) section 3.1, [agent/07.5-what-a-claim-and-a-give-back-send.md](agent/07.5-what-a-claim-and-a-give-back-send.md)) once the server sends tasks.

## 2. Decisions

- **[Decided]** The computer polls for tasks and runs what the server sends. It starts nothing the server did not send.
- **[Decided]** A task is an item's whole chain.
- **[Decided]** The computer reports its health and a `task_end` for every task.
- **[Decided]** Fail closed: a plan lane cut off from the hub starts nothing.
- **[Decided]** Settings come from the project's store, not from a local copy.
- **[Decided]** A piece build that was refused or lost is asked for again.
- **[Decided]** Among computers, the one with the most room on its Claude week is preferred, and a waiting plan is left to the computer that has the week to spare.
- **[Proposal]** The poll interval is the loop's `pollIntervalSec`; a `work_available` message on the cloud link ends the wait early, as in section 7.5.
- **[Proposal]** Task and report field names follow the TypeScript agent's (`task`, `task_end`, `health`); the implementer copies them from there rather than from this spec.

## 3. Behavior, in build order

1. **Poll and run a task** (`src/orchestrator/driver/pool.rs`, new). Per project, when a lane is free, ask the store for the pool's next task. A task names the item, the kind of chain and the `tier` ([agent/07.1-model-tiers.md](agent/07.1-model-tiers.md): `opus`, `sonnet` or `local`; unknown reads as `sonnet`). The agent runs it through the existing implementer, checks and review steps and holds the lane for the whole chain. A `{ waitingFor }` answer (missing tags) stops the polling until `work_available` or a tag change, as before.
2. **Report health and end.** The computer reports its health on each tick (lanes free, sessions running, the Claude week's room: section 9.2) and, when a task ends, sends `task_end { task, outcome, result }`. A failure whose cause isn't the work's says so (`outside`, [pieces.md](pieces.md) section 3.6). A refusal is sent again a minute later; a `stale` answer drops the work.
3. **Fail closed when the hub is cut off.** A plan lane (a lane serving a plan or draft task) whose connection to the hub is down starts nothing new, and logs one `hub_cut_off` event until it is back. Running tasks are not killed by this; they end as usual. No local fallback and no local choosing of work.
4. **Settings from the project's store.** A task's run reads the project's settings (needs, checks, engines, limits) from the store at the start of each task, not from `.retaskable/config.json` or `state.json`. If the store can't be reached, the task does not start.
5. **Fix and job tasks.** A `fix` task (a failed check or a red CI on an item's PR) and a `job` task (a chore, [agent/06.7-chores-never-idle.md](agent/06.7-chores-never-idle.md)) arrive the same way and are reported the same way. The agent runs no schedule of its own for either.
6. **Ask again for a piece build.** A piece build the computer could not start or lost (the lane went, the engine was out, the take was refused) is asked for again on the next poll, not dropped, until the server says it is taken elsewhere.
7. **Room on the Claude week.** The health report carries the week's room (`seven_day` utilization and reset, [agent/07.1-model-tiers.md](agent/07.1-model-tiers.md)). When several computers are polling, the server prefers the one with the most room. The agent's part is only to report it truthfully and to start no task from a week already at the cutoff.
8. **A waiting plan waits for the computer with the week to spare.** A plan task that is waiting (for a limit reset, say) is not taken by a computer short of room on its week: it declines the poll for that task and leaves it to a computer with the week to spare. A computer with no readings counts as having room.
9. **Tier mapping.** The task's `tier` maps to the engine's `--model` as [agent/07.1-model-tiers.md](agent/07.1-model-tiers.md) says, with the local fallback to `sonnet` when the local model is off or down. This is the same function `src/tiers.rs` already has; the task path calls it, and no tier is worked out from usage.

## 4. Tests

- **Task received**: a poll answer with a task starts the chain on a free lane with the tier named.
- **Task ended**: the chain's end sends `task_end` with its outcome; a refusal is retried; `stale` drops.
- **Hub cut off**: a plan lane with no hub starts nothing and logs once; it starts again when the hub is back.
- **No room**: a computer at the weekly cutoff starts no task, and a waiting plan is declined to leave it to another.
- **Tier mapping**: `opus`, `sonnet`, `local` (available and not), and an unknown name.
- **Settings**: a store that can't be reached starts nothing.

## 5. Acceptance

- With the server sending a task, the computer runs the whole chain and reports `task_end`; with nothing sent, it starts nothing.
- Cutting the hub on a plan lane starts nothing, and no local fallback appears.
- Settings changed in the store apply to the next task with no restart.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
