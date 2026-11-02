## 6. Reporting to the host

The agent posts a commit status through the code-host interface (code-storage.md section 3.1) to the exact commit, with a new capability, `commitStatus` (GitHub: `gh api -X POST repos/{o}/{r}/statuses/{sha}`; GitLab: `glab api -X POST projects/:id/statuses/{sha}`), run as an argument list, never a shell.

| Moment | State (GitHub / GitLab) | Description |
|---|---|---|
| A computer saw the commit and asked for a run | `pending` / `pending` | "Queued: waiting for a computer to run the checks" |
| A computer took it | `pending` / `running` | "Running on <computer>: 2 of 5 checks done" (updated at most once a minute, and on each check's end) |
| All blocking checks passed | `success` / `success` | "5 checks passed in 3m 12s" (or "...; 1 non-blocking check failed: lint") |
| A blocking check failed | `failure` / `failed` | "test failed (exit 1). 4 of 5 checks passed" |
| CI could not run or finish: no approved checks, an unclean checkout, a timeout, a machine failure after one rerun (the `ciHealth` rule) | `error` / `failed` | The cause, in a line: "No checks are set up for this project" |
| Cut by a restart or a computer that went away | stays `pending` / `running`, description "Interrupted: running again" | Never `success`, and not `failure` either (section 13) |
| Cancelled because the commit was replaced | GitLab `canceled`; GitHub has no such state, so it stays `pending` and the next commit's status is what counts | "Replaced by a newer commit" |

- `context` is `retaskable/ci` (`name` on GitLab). `target_url` is the run page (section 7).
- A status is posted by the computer that holds the run's lease. A run's state transitions are plain code in the agent; no LLM is asked anything.
- **Platform jobs**: a run with checks needing different platforms is split by the store into **jobs**, one per set of needs, which different computers may take. Only the computer that finishes the last job posts the final state, from the store's record of every job (the store answers "all jobs done?" in the same op as the job's own finish, so two computers finishing together cannot both be last). If any job failed, the run fails when all are done, or at once when `failFast` is on (a team setting, default off).
- **Rate and size**: statuses are limited per commit and context by the hosts (GitHub: 1000). A run posts one at the start, at most one a minute while running and one at the end. `description` is cut to the host's limit (140 characters on GitHub).
- **A failed post** (the host is down, the login lacks scope) is retried like any host error (a minute, five in a row), then the run's record says "the host wouldn't take the status", the run page shows the result, and the agent's own merge uses the store's record for the agent's own pull requests (section 9) so a host outage doesn't strand a passing change. A login without status scope shows the card line "gh can't post statuses: run `gh auth refresh -s repo:status`".
