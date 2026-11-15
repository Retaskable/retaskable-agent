# The merge queue from the store — Spec

Status: v0.1, draft (2026-11-11). Backlog: `merge-queue-from-store`. Copies Retaskable.com's [merge-queue-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/merge-queue-on-server.md) (sections 5, 6 and 6.6, the agent's part only) and the TypeScript changes #3399, #3404 and #3446. Builds on [pieces.md](pieces.md) (the `merge` run, section 3.4, and the locks, section 3.7), [agent/07-orchestrator-loop.md](agent/07-orchestrator-loop.md) and [agent-telemetry.md](agent-telemetry.md).

Theme: Orchestration

Kind: new

## 1. What it is

The store owns the merge queue: the changes waiting to merge into a base, whose turn it is, the order, and the lease on the turn. The computer asks the store for its turn and does only the git and code-host work it is told: bring the branch up to date, run the checks, push, merge, or say why it can't. It never decides who merges next, and it never merges without a turn the store has just confirmed. This replaces the `merge-<base>` lock a `merge` run takes today (`lock_take`), and the local merge turn in `lane_pool.rs` (`merge_turn`, `set_merge_turn`, `lane_for_merge`), which has the computer guess whose turn it is.

A computer that can't reach the store starts no merge and takes no turn (fail closed): there is no "merge anyway" wait.

## 2. Decisions

- **[Decided]** The store's queue decides whose turn it is; the order, the lease and the caps are the store's (the lease numbers are the TypeScript agent's: stale after 5 minutes without a beat; at most 2 hours 15 minutes in `ci` or `auto_merge`, 45 minutes in any other phase).
- **[Decided]** A piece's merge run is the entry: it is taken with `merge_next { piece: true }`, never `run_take`, beaten with `merge_beat`, and ended with `merge_result`, never `run_beat` or `run_done`. One lease, the turn's.
- **[Decided]** A chore with no store item joins the queue with `merge_enqueue { chore: true }`, which queues it by its id without looking the item up (without it the entry never queues: "No item chore-N").
- **[Decided]** The `claim` telemetry event also covers `run_take`, so what the store answered is never hidden by "not asked" ([agent-telemetry.md](agent-telemetry.md) section 3).
- **[Proposal]** The Rust names for the store's ops are `Client::merge_enqueue`, `merge_withdraw`, `merge_next`, `merge_beat`, `merge_open` and `merge_result` in `src/backlog/`, beside the run ops.
- **[Decided]** `LanePool` keeps no lane for a turn's holder (`merge_turn`, `set_merge_turn` and `lane_for_merge` are gone, and `free_lane` takes no asking item): a merge run takes no lane, and the lane choice stays the computer's, the turn the store's.

## 3. Behavior, in build order

1. **The ops** (`src/backlog/`, all agent ops, tests against the fake store that `src/backlog/runs/tests.rs` uses):
   - `merge_enqueue { item, piece?, base, head, approvedSha?, pr, land, chore? }` → `{ entry, place }`. Idempotent: a second call for the same change updates `head` and keeps its place. Sent when a change enters `merging`.
   - `merge_withdraw { item }` → `{ ok }`, when the change leaves `merging` for another reason (sent back, needs a person, skipped, closed). The store also withdraws it when it moves the item out of `merging`.
   - `merge_next { base, piece? }` → `{ turn: { entry, token, leaseMs } }`, `{ waiting: { place, ahead } }` or `null` (none of this computer's changes is queued). Asked again while it holds the turn, it returns the same turn. A piece's merge is handed out only here: when the store marks a piece ready with `next: merge`, the entry exists already and the answer carries what `run_take` answers for a merge (the piece, its change, the token).
   - `merge_beat { token, phase }` → `{ held }`. Every 60 seconds while the computer works the turn or waits on CI or auto-merge. `held: false` means stop: nothing more is pushed or merged.
   - `merge_open {}` → `{ turn? }`: this computer's held turn, if any.
   - `merge_result { token, outcome, … }` → `{ held, next? }`, outcome one of `merged { mergeCommit }`, `updated { head, approvedIntact }` (`approvedIntact: false` is released as sent back), `handed_off` (auto-merge on: the turn is kept until merged), `ci_failed { checks }`, `sent_back { reason }`, `needs_person { reason }`, `closed`, `retry { afterSeconds }` (the host wasn't ready or the mergeability is unknown; past the retry limits the computer reports `needs_person`) and `released` (no verdict; the entry keeps its place). A result from a former holder is dropped and answered `{ held: false }`, except `merged`, which is a fact about the host and is always recorded.
2. **A merge from an item** (`src/item_lifecycle.rs` and the merging step; this agent has no step that lands an item's change yet, so no code sends these ops for an item: the ops of step 1 are what that step calls once it is ported): `merge_enqueue` on entering `merging`; `merge_next` for its turn; while it `waiting`, the timeline note "Waiting for <id> to merge first" (the store's `place` and `ahead`); `merge_beat` before every push and before `gh pr merge`; the merge passes the head the checks passed (`--match-head-commit`), so a holder that lost its lease in between is refused by the host. The result goes out as `merge_result`; the turn is held through CI after an update (`updated`, phase `ci`) and through auto-merge (`handed_off`). Red checks or CI are `ci_failed`: the turn is released at once.
3. **A piece's merge run** (`src/orchestrator/merge_run.rs`): instead of `lock_take` of `merge-<base>`, `merge_next { piece: true }`. A `waiting` answer asks again every 15 seconds, as the lock wait did, up to the ceiling less its margin, then ends the run `interrupted` naming what it waited for. The turn's token goes in `RunLocks` (`src/runs/locks.rs`) so the loop beats it with `merge_beat`; `RunLocks::held` is checked before updating or landing, as it is now, and a lost turn ends the run with nothing pushed or merged. The run ends with `merge_result` (`merged` → the piece's `merged`, `updated` → `in_ci`, a conflict or a refusal → `sent_back`, back to `fix`), which also ends the run in the store and releases the turn in one step; `run_done` and `run_beat` carry no merge tokens, and no `lock_drop` is sent. A run that dies mid-merge goes back to `merge` as attempt 2, not counted against the piece's attempts; the next holder reads the host first and reports `merged` if it landed. The rules of `pieces.md` section 3.4 about `behind`, `gh pr view`, the `sync_main` merge commit and the 15-second re-reads are unchanged.
4. **A chore's merge, with no store item** (a chore the computer holds as its own record; a chore that is a store piece merges as a piece, step 3, and as one it has a lane-free turn like any other change): `merge_enqueue` with `chore: true`, then the same turn, beats and results as step 2.
5. **Restart** (`runs_drop`'s moment, before any `run_take`): the computer calls `merge_open`. A held turn is resumed only after reading the change on the host: already merged → report `merged`; its head moved since `approvedSha` → carry on as after an update (CI again); otherwise from the phase. With no turn, or a lease already gone, it goes back to `merge_next`. It never rebuilds a turn from `state.json`. Built in the run loop's first passes (`src/orchestrator/driver/runs.rs`): once after `runs_drop` and before any ask, for a computer that can start merge runs, `merge_open`; a store that can't answer it gets no ask this pass and `merge_open` again on the next. A held turn with a piece starts its merge run again with that turn's token, and the run reads the change on the host before it pushes or merges (`merge_run.rs`: merged → `merged`; behind or conflicting → updated as in step 3; otherwise landed); a held turn with no piece is given back `released`.
6. **Away from the store:** a computer that holds a turn and loses the store finishes the host call it is in the middle of and keeps its `merge_result` to send when the store is back (the run's end stays pending and is sent again a minute later, as a `run_done` is). A result the store drops because the turn is no longer held, `merged` apart, is logged. If the lease ran out first, a late `merged` is accepted, any other result is dropped with a log line, and nothing more is pushed.
7. **Logging:** the `claim` event is written for every `run_take`, `merge_next` and `merge_open` answer, and for `claim_next`, with `op` naming which, and `answer`: the item or piece id (the piece's title for a run), `none`, `passed`, `waiting_for_needs`, a `waiting` place or `failed`.
8. **A red-main test** (`src/orchestrator/driver/tests/runs/restart.rs`): a red main's fix-main chore still goes through the queue like any other change: its merge takes `merge_next` and ends with `merge_result`, and no `merge-` lock is taken.

## 4. Acceptance

- A `merge` run takes no `lock_take`; it asks `merge_next`, and a store that can't be reached ends it without a merge.
- Two computers on one base never hold a turn at once (checked in the fake store: the second gets `waiting` until `merge_result` or a lost lease).
- A change that waits shows "Waiting for <id> to merge first".
- No `merge_beat` returning `held: false` is followed by a push or a merge.
- A chore with no store item joins the queue and merges.
- A restart with a held turn calls `merge_open` and reads the host before acting.
- `ci_failed` releases the turn at once.
- The `claim` telemetry event shows each `run_take` answer.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
