# A restart waits for a running TestFlight build — Spec

Status: v0.1, draft (2026-11-11). Backlog: `testflight-restart-wait`. Copies the TypeScript changes #3241 and #3300 (https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs: agent/07-orchestrator-loop.md, testflight-release.md section 3.3). Builds on [restart-and-self-update.md](restart-and-self-update.md) and [agent/07.4-stopping-and-restarting.md](agent/07.4-stopping-and-restarting.md).

Theme: Orchestration

Kind: new

## 1. What it is

The agent restarts to update after nearly every merge, and a restart used to kill a TestFlight build part-way, with no result, so the site showed "Building" for ever. A restart now waits for a running build. Beside it, a project whose merges go straight to production always gets the production-only view of its TestFlight read, with no flag.

## 2. Decisions

- **[Decided]** A restart lists "a TestFlight build" among what it finishes first, for at most 45 minutes (`TESTFLIGHT_FIRST_MS`), then goes even if the build hangs. Quit when idle waits for it with no cap.
- **[Decided]** While a restart waits only on rollouts or a TestFlight build (the long waits), implementers keep starting.
- **[Decided]** Under Production only, the `environments` read answers a production-only view (no staging head or serves, `productionHead` the default branch's head, `productionServes` what production's version URL answers, `behind: []`, `behindCount: 0`, `blocked` "This project's merges go straight to production: there's nothing to promote.", no rows). This is on always; the agent reads no flag ([no-agent-flags.md](no-agent-flags.md)).
- **[Proposal]** The wait is a hook, `testflight::testflight_running()` (`src/testflight.rs`), true while any `testflight::Running` guard is held. The Rust agent has no TestFlight build of its own yet, so nothing takes a guard and the hook is false; whatever later runs a build holds a `Running` for exactly as long as the build runs. Porting the build, and the `environments` read's production-only answer, is outside this item.

## 3. Behavior, in build order

1. `testflight_running()` and the `Running` guard (`src/testflight.rs`): in the code; a build takes a guard when it is ported.
2. In the code: the restart's waiting list (`src/restart.rs`) and quit's gain "a TestFlight build"; a wait that is only rollouts or builds counts as long; the finish-first cap is 45 minutes while a build runs.
3. The `environments` production-only answer, as in section 2, without a flag.
4. **Tests:** the waiting list with and without a build; the long-wait rule; the 45-minute cap; the production-only answer's fields. Ends with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## 3.1 Details the wait settles

What `src/restart.rs` does today, then what is still open. Everything below that is not a description of the code is a **[Proposal]**.

**As built**

- `RestartState::held()` lists what is held (a merge, a promote), then "a TestFlight build" (`TESTFLIGHT`) while `build_running` (default `testflight_running`; a test swaps it) is true. Quit reads the same list.
- `finish_first` reads the cap on every 100 ms poll: `TESTFLIGHT_FIRST` (45 minutes) while the list has the build, otherwise `FINISH_FIRST` (2 minutes). The cap counts from when the wait started, not from when the build did. A build that starts mid-wait stretches the cap to 45 minutes; one that ends mid-wait drops it back to 2, so a wait already past 2 minutes at that moment ends at once.
- The wait ends the moment the list is empty, or at the cap. Then the step is `now`, and the sequence goes on: cut sessions, stop other sessions, exit 75.
- `report()` shows `{ step: "waiting", waitingOn: ["a TestFlight build"], at, by, reason }` while it waits, so the site can say what a restart is waiting for; the step turns `now` after.
- Telemetry: `restart_waiting` is logged once, with the list the wait started with and the milliseconds waited when it ended.

**Open edges**

- **Cut after 45 minutes.** The build is killed with every other session; nothing writes a result. **[Proposal]** Whatever runs the build also records `failed`, "Cut by a restart after 45 minutes", when it finds its guard dropped by a restart, so the site never shows "Building" for ever (the bug this item exists to stop). Left to the build's own item.
- **A second restart while one waits** keeps the first's reason (`ask` returns false) and does not reset the 45 minutes.
- **A build and a merge held together.** The 45-minute cap applies to the whole list, so the merge may be waited on past 2 minutes. That is intended: the long wait is for the build.
- **Implementers keep starting** while the list holds only rollouts or a build: `stopping()` is set only by the cut, after the wait. The slice-5 gate (`update-pending-gate` in [restart-and-self-update.md](restart-and-self-update.md)) blocks them only for an `update` restart, and that is unbuilt.
- **What a person sees.** The settings page's restart line reads `waitingOn` as is; no new text is needed.

## 4. Acceptance

- The 45-minute cap counts from the start of the wait, and follows the build: a build ending mid-wait returns to the 2-minute cap (`src/restart.rs` tests).
- A restart does not kill a running TestFlight build before 45 minutes; one that hangs is cut then.
- Implementers keep starting while that wait runs.
- A production-only project's TestFlight read always returns the production-only view.
