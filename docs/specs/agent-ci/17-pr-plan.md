## 17. PR plan

Each piece is about 500 changed lines including tests, in the order they can be built. Pieces 1 to 5 need nothing from the store; 6 onward need the companion's store ops (section 14 items 2 and 3), or a fake store.

1. **The setting and its default.** `ci` in the agent's config and `projectSettings` reader, mapping from `requireCi`, `ciCoversChecks`, `ciWhenReady`; the detection of "own CI present" (coverage and required checks); tests on temp repos and a faked host. No behaviour change yet.
2. **The clean checkout.** The mirror, `git fetch <sha>` and pull-request refs, the detached worktree, submodules and LFS, the clean-state assertions and teardown, the leftover cleanup on start; tests against real temp git repos including an untracked and an ignored file in the project's own checkout that must not appear.
3. **The check runner.** Build the command list from approved `checks` or `ciChecks` (and refuse unapproved ones), per-check `needs`, the scrubbed environment and `RETASKABLE_*` variables, timeouts, process-tree kill, log files with the size cap and 7-day sweep, the result record; tests with fake commands (exit codes, timeouts, a runaway child).
4. **The status poster.** `commitStatus` on the GitHub and GitLab code hosts (argument-list `gh api` / `glab api`), state mapping, description building and cutting, throttling and retry, the missing-scope message; tests with a faked CLI.
5. **Dependency caches.** The kind and key rules from lockfiles, the environment variable per tool, the copy-from-latest, the eviction and size cap, and "nothing lands in the worktree"; tests on a temp tree.
6. **Runs in the store.** The `ci` run kind: `ci_request`, `ci_take` with needs and slot, lease and beat, `interrupted` on start, the three-interruptions error, jobs for platform splits, the last-job rule, and the warm-affinity grace; against a fake store (the real ops arrive with the companion).
7. **Triggers.** The poller (own ready change replacing the label, fix pushes, base head, people's trusted pull requests), supersede and cancel, draft and item-branch exclusions, the first `pending`; tests on a faked host.
8. **The gate.** `awaiting_ci` and `merging` reading `retaskable/ci` for a `retaskable` or `both` project, the 2-hour pending limit, `ciHealth` and `ci_rerun`, the fix follow-up reading a log tail by `ci_log`, `merge_pr` refusing red or pending, and fix-main reading the base's status; tests on the existing orchestrator doubles.
9. **Untrusted pull requests.** Trust classification, the approval wait, `ciUntrusted` at the computer, the container requirement, read-only trusted-cache seeding; tests for each author case, a fork and a stale approval.
10. **Live logs.** Streaming a check's log to the page over the cloud link, `ci_log` as a read-only command with its limits, the offline answer; tests with a fake link (no log ever sent in a store call).
11. **The computer's settings.** `ciSlots`, `ciUntrusted`, cache size and the CI card on the settings page (agent-settings.md), the disk sweep, and the queue's staging-branch run (event `queue`) and stale cancel hooked up once #2407's agent side exists.

The Retaskable.com side (section 14) lands in parallel: items 1 to 3 before piece 6, item 4 and 5 before piece 10.
