## 12. Interplay

**With ci-when-ready ([ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md))**: `ciWhenReady` and the `ci-ready` label exist to keep a host CI from running before a change is ready. With `ci` of `retaskable` the agent decides when to ask for a run, so neither is needed: "ready" (pushed, section 5 of that spec) requests the run. The label stays for `own` and `both`. `ciCoversChecks` has a counterpart: for a `retaskable` project a team setting, `ciCoversChecks`, may skip the lane's pre-review checks that CI runs beside the review (the same trade as before). It stays off by default.

**With the merge queue (#2407)**
- **Item pull request**: CI on the item's pull request to the base is a `retaskable/ci` run on its head (the agent's own change ready). A piece's pull request into the item branch has none (section 8).
- **The staging branch.** #2407 section 11 has Trigger A (a push to `merge-queue/<base>` that the project's own CI is configured to run on) and Trigger B (a draft pull request). With `retaskable` CI, there is no trigger to configure: the holder pushes the branch and asks for a run on that commit (event `queue`), and reads the `retaskable/ci` status at that sha with the same `commitChecks` call it uses for any CI. "CI doesn't run on pushes here: Needs setup" (its section 13) cannot arise for a `retaskable` project. For `both`, the holder waits for the staging commit's other checks as it does for any CI.
- **Cancelling stale staging runs**: its `ciRuns.cancel(branch, exceptSha)` for Retaskable CI is a store op that ends the run `cancelled`; the computer holding it kills the process tree. No CI API token, no per-CI code.
- **Reds**: the bisect works unchanged on the status. A red whose cause is a machine failure (`ciHealth`) reruns the same commit; here that is `ci_rerun`, free.
- **Offline holder, restart**: the queue's rule that the next holder finds the staging commit's result and continues applies: the run's result is in the store, the status is on the host.
- **Combination tests and the lone entry**: a lone queued entry that "merges as it is" (its section 10.1) has already had its own `retaskable/ci` run on its head; there is no staging run to wait for.

**With restart ([agent.md](../agent.md) section 7.4, `restart-cuts-sessions`)**: Section 13.

**With `own` and `both` projects' deploys.** Out of scope, section 16.

**With the existing Woodpecker for Retaskable's own repos**: they stay on `own` until the queue ships, because `seen`, `turn` and `merge` are Woodpecker-specific coordination that the queue replaces. Moving them to `retaskable` is a later step, and not part of this spec's PRs.
