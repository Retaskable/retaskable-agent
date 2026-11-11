# Catching up with the TypeScript agent (week to 2026-11-11)

Status: a to-do list, from Retaskable.com's `agent/` and `shared/` commits of the last 8 days (314 files changed in `agent/`). Each item was checked against `src/` here with greps only, not by reading the code. Tick items off as they land.

## A. Delete code the TypeScript agent removed

- [ ] A1. Galleries: the gallery config and settings, the commands (`shoot`, `gallery_*`), the core and the public uploads. Here: `src/galleries.rs`, `src/galleries/`, spec `06.8`, mentions in `commands`, `command_gate`, `allowed_commands`, `paths`, `drafts/prompt`.
- [ ] A2. The ideas round: the daily round, `ideas.json`, `ideas_offer`, the `no-claude-ideas` flag. Here: `src/ideas/`, `orchestrator/driver/ideas_round.rs`, `ideas_daily.rs`, `vet_daily.rs`.
- [ ] A3. `src/pace.rs`: the agent's own pacing goes (pools PR 7), only the hard cutoff stays. The server picks the tier and the computer maps it to a model.
- [ ] A4. `src/jobs_due.rs`: TS deleted `jobsDue.ts`. The server issues jobs; the agent runs Lighthouse, probes and sweep tasks it is sent.
- [ ] A5. Offers and caches: settings, facts, releases, logs, item details, draft details, orphan items, orphan drafts, sign-offs (`approvals_offer`); the local backlog and `playtestQueue` move-ins; the `choresStarted`, `manualApprovals` and `releaseNote` caches. Check `release_notes/saved.rs` and `decisions.rs`.
- [ ] A6. `remote.log`: stop writing it (the cloud's `audit_log` is the record). Still in `commands.rs`, `connector/mod.rs`.
- [ ] A7. Flags: `agentFlags.ts` and the gates `ci-watch-fix-red`, `ci-watch-conflicts`, `rollout-batch-bisect`, `release-fewer-settings`, plus the per-computer rollout runner. Review `src/flags.rs`, `ci_watch`, `agent_ci`.
- [ ] A8. Old rollout code (`rollout.ts`, `rolloutFact`, `rolloutFixed`, `rolloutHarness`, lease and takeover tests, the fixed-schedule parameter), `orphans.ts`, `mergeBatch.ts`.

## B. Add what TS built

- [ ] B1. Rollout on the server (the computer reports and obeys): Production reports, polls and rolls out like a staging project; log each report, each change in the poll answer and each result; read CI from commit statuses only when the token can't read check-runs; an idle poll says why and names what holds the release; never release a commit production already has; an old waiting try-out no longer holds every release; seed past a crashed running fact after an hour; rate limit rollout health; deploy accepts a newer uploaded version; retaskable.com's own traffic and health commands come back, a saved site setting runs nothing. TS files: `rolloutRun`, `rolloutReport`, `rolloutDeps`.
- [ ] B2. Pools: run the tasks the server sends (a task is an item's whole chain), report `health` and `task_end`, the plan's lanes cut off from the hub start nothing, settings move to the project's store, the server picks the tier, ask for piece builds again. TS files: `poolTasks`, `poolReport`, `fixTasks`.
- [ ] B3. Merge queue: take the turn from the store; piece merge runs and chores take it without a store item; test that main is red; log `run_take` answers.
- [ ] B4. Chores: the store issues the playtest fix and the smoke fix; `chore_nofix` is a store fact; the store refuses duplicate by-hand chores; stage times and `heldSince` are store facts.
- [ ] B5. Jobs: port `jobTasks`.
- [ ] B6. CI watcher: the store decides verdicts (`ci_result` op); slower watch and back-off on the rate limit (`ghLimit`); re-read the login after a failed read; don't remember a rate-limited access check; merge main into a conflicted PR, retrying only when main moves; detect, report and fix red PRs, including the Fix button for another person's red PR; Run now through the store; the Fix button waits for the first attempt.
- [ ] B7. Plans and questions: a failed plan always says why and Try again uses it; a dropped plan reaches the store and Approve says so; Build this on an idea already under way makes no second draft; `optionsFill` for a question with no options (only the one being evaluated says so); name the item on per-item chat messages; `triageTask` and `classify`; pass `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` through in Docker where it applies.
- [ ] B8. Small items: a restart waits for a running TestFlight build; TestFlight production-only fixes always on; server-issued work goes to the computer with the most room on its Claude week; a waiting plan is left for the computer with the week to spare; bug reports never cut half an emoji; `usageCheck`, `keepAwake`, `nativeApp`, `ideaBugSort`.

## C. Spec sync

- [ ] Copy the changed specs into `docs/specs`: rollout on the server, merge queue, pools and pacing, chores, scheduled jobs, `agent-settings`, `agent-telemetry`, the `agent/` folder. Refresh `TRIMMED.md`.

## Suggested order

A1, A2, then A3-A8 (one PR each); B6; then pools, merge queue and chores together; rollout last, since it depends on the store ops. Read the TS diffs for B1-B4 before porting.
