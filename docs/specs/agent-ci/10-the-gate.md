## 10. The gate

**The agent's own merges** wait on `retaskable/ci` at the head, itself: `awaiting_ci` reads the commit's checks (`CodeHost.checks` / `commitChecks`), which now includes the status, so for a `retaskable` project that path is unchanged and works whether or not the host enforces anything. The pending, failure and error handling there (fix runs, `ciHealth` machine failures, "CI failed on this commit too" in a review send-back) apply as they do now. With `ci` of `retaskable` or `both`, the agent never merges a change whose head has no `success` for `retaskable/ci`.

**Setup tells the owner to make the check required** on the base branch:
- GitHub: Settings, Branches (or Rulesets), "Require status checks to pass", add `retaskable/ci`. A repo on a free plan that is private has no branch protection at all (GitHub only offers it on paid plans or public repos); Setup says so.
- GitLab: Settings, Merge requests, "Pipelines must succeed" does not apply to external statuses on the free tier; Setup shows the external status check setup where the plan has it, and otherwise the same note.
- The check reads the base branch's protection through the `rules` capability. The project's board shows "`retaskable/ci` isn't a required check" with the steps, until it is.

**What happens if they don't**: the agent's own merges are still gated (above). What is not gated is a person's merge on the host's button, which GitHub allows over a red status when nothing is required. The status is still red on the pull request, the site's own **Merge** (`merge_pr`) button is greyed with "CI hasn't passed", and the agent's `merge_pr` handler checks the status again and refuses a red or pending head (a person may override with a fresh sign-in, and the override is noted in the item's history). So the unprotected case degrades to "visible and discouraged", not "unchecked".

**`both`**: the merge waits for `retaskable/ci` and for every other check on the head, reading the host's checks as it does now. The project's own checks may also be required by the host as before.
