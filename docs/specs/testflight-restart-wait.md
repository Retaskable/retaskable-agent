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
- **[Proposal]** This applies once the Rust agent has a TestFlight build and an `environments` read; neither is in the code today, and porting them is outside this item. The wait is a hook (`testflight_running()`, true while any build runs) that is false until then.

## 3. Behavior, in build order

1. `testflight_running()` in the module that will run the build.
2. The restart's waiting list (`src/restart.rs`) and quit's gain "a TestFlight build"; a wait that is only rollouts or builds counts as long; the finish-first cap is 45 minutes while a build runs.
3. The `environments` production-only answer, as in section 2, without a flag.
4. **Tests:** the waiting list with and without a build; the long-wait rule; the 45-minute cap; the production-only answer's fields. Ends with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## 4. Acceptance

- A restart does not kill a running TestFlight build before 45 minutes; one that hangs is cut then.
- Implementers keep starting while that wait runs.
- A production-only project's TestFlight read always returns the production-only view.
