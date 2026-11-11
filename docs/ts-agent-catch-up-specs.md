# Catch-up pieces, one spec each

Status: drafts, to go to Retaskable. Each piece is one PR with its tests, ported from the TypeScript agent (`Retaskable/Retaskable.com`, `agent/` and `shared/`) and checked off in [ts-agent-catch-up.md](ts-agent-catch-up.md). Behaviour is changed in Retaskable.com's specs first; the Rust agent copies it. Every piece ends with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

Order: P1-P8 (removals, independent), P9, then P10-P12, then P13 (rollout), P14 (specs) alongside.

## Removals

### P1. Remove galleries
Goal: the agent takes no pictures and uploads no galleries.
Do: delete `src/galleries.rs`, `src/galleries/`, the gallery commands (`shoot`, `gallery_*`) and their entries in `commands`, `command_gate`, `allowed_commands`; drop the gallery config and settings, the gallery lines in `paths` and `drafts/prompt`, and spec `06.8`.
Done when: no `galler` match in `src/` or `docs/specs` except history; a received `shoot` or `gallery_*` command is refused as unknown.
TS: #3231, #3252, #3287, #3291, #3330.

### P2. Remove the ideas round
Goal: the agent keeps no ideas file and runs no daily ideas round.
Do: delete `src/ideas/` store and write paths, `orchestrator/driver/ideas_round.rs`, `ideas_daily.rs`, `vet_daily.rs`, the `ideas_round` command; keep only what accepting an idea from the store needs (`ideas/accept.rs`).
Done when: nothing reads or writes `ideas.json`; the store gets no ideas offer.
TS: #3275, #3310, #3327, #3329, #3335.

### P3. Remove the agent's own pacing
Goal: the server picks the tier; the computer only maps it to a model and keeps the hard weekly cutoff.
Do: cut `src/pace.rs` down to the hard cutoff; `tiers.rs` maps a tier named by the server to a model.
Done when: no local pacing state is saved or restored; tests cover the cutoff and the tier-to-model map.
TS: #3417, #3410. Depends on P9 (the server sends the tier).

### P4. Remove the local jobs-due check
Goal: the server issues scheduled jobs; the agent runs the task it is sent.
Do: delete `src/jobs_due.rs` and its callers in `backlog.rs` and `orchestrator/driver/deprecations.rs`; run Lighthouse, probes and sweep tasks from the server's task (see P9).
TS: #3422, #3426.

### P5. Remove the offers and caches
Goal: the agent offers the store nothing it can read from there.
Do: drop the offers for settings, facts, releases, logs, item details, draft details, orphan items, orphan drafts and sign-offs; the local backlog and playtest-queue move-ins; the `choresStarted`, `manualApprovals`, `releaseNote` caches (`release_notes/saved.rs` offered file, `decisions.rs`).
Done when: the store client has no `*_offer` call; tests for each removed offer are gone.
TS: #3336, #3338, #3348, #3360, #3363, #3364, #3365, #3367, #3376.

### P6. Stop writing `remote.log`
Do: remove it from `commands.rs`, `connector/mod.rs` and their tests; the cloud's `audit_log` is the record.
TS: #3192.

### P7. Remove flag gates
Do: delete `ci-watch-fix-red`, `ci-watch-conflicts`, `rollout-batch-bisect`, `release-fewer-settings` gates (always on or gone, as TS decided); review `flags.rs`; add the lint check that no agent flag is read (TS #3325).
TS: #3219, #3222, #3209, #3325.

### P8. Remove the old rollout code and orphans
Do: the per-computer rollout runner, the fixed-schedule parameter, orphan handling and the merge batch, if present in `src/release*`, `merge_update.rs`.
TS: #3349, #3209. Do together with P13 if the code is shared.

## Additions

### P9. Pools: run what the server sends
Goal: work comes from the server; a task is an item's whole chain.
Do: poll for tasks; run them; report `health` and `task_end`; plan lanes cut off from the hub start nothing (fail closed); settings come from the project's store; ask for piece builds again; prefer the computer with the most room on its Claude week; leave a waiting plan to the computer with the week to spare.
Tests: task received, task ended, hub cut off, no room, tier mapping.
TS: `poolTasks`, `poolReport`, `fixTasks`, `jobTasks`; #3405, #3400, #3408, #3409, #3447, #3478, #3190.

### P10. Merge queue from the store
Goal: the store's queue decides whose turn it is.
Do: take the turn from the store (`run_take`), for piece merges and for chores without a store item; log the answers; a red-main test.
TS: #3399, #3404, #3446. Touches `lane_pool.rs`, `orchestrator/merge_run`.

### P11. Chores from the store
Do: the store issues the playtest fix and the smoke fix; `chore_nofix` is a store fact; stage times and `heldSince` are store facts; the agent never makes a duplicate by-hand chore.
TS: #3425, #3380, #3466.

### P12. CI watcher through the store
Do: `ci_result` op decides verdicts; slower watch, back-off on the GitHub rate limit, no remembered rate-limited access check; re-read the login after a failed read; merge main into a conflicted PR, retry only when main moves; detect, report and fix red PRs, including another person's (Fix button waits for the first attempt); Run now through the store.
TS: `ciWatch`, `ghLimit`; #3467, #3255, #3175, #3165, #3164, #3161, #3458, #3443.

### P13. Rollout on the server
Goal: the computer reports and obeys; the store's machine decides.
Do: new `rollout` module: report, poll, act; Production behaves like a staging project; log each report, change and result; read CI from commit statuses only if check-runs aren't readable; an idle poll says why; never release a commit production already has; an old waiting try-out doesn't hold releases; seed past a crashed running fact after an hour; rate-limit the health check; deploy accepts a newer uploaded version; retaskable.com's own traffic and health commands; pass `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` through.
TS: `rolloutRun`, `rolloutReport`, `rolloutDeps`; #3401, #3463-#3475, #3444, #3457, #3459. Biggest piece; split per bullet group if needed.

### P14. Plans, questions, and small items
Do, as separate small PRs: failed plan always says why and Try again uses it (#3314); dropped plan reaches the store (#3290); Build this on an idea under way makes no second draft (#3250); `optionsFill` (#3316, #3388); name the item on chat messages (#3305); `triageTask`, `classify`; restart waits for a running TestFlight build (#3241); TestFlight production-only fixes always on (#3300); no half emoji in bug reports (#3358); `usageCheck`, `keepAwake`, `nativeApp`, `ideaBugSort`.

## Specs

### P15. Sync `docs/specs`
Copy the changed Retaskable.com specs (rollout, merge queue, pools and pacing, chores, scheduled jobs, `agent-settings`, `agent-telemetry`, `agent/`), then refresh `TRIMMED.md`. Land each spec with its code piece where possible.
