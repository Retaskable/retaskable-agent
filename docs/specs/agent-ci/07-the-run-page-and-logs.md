## 7. The run page and logs

A run page on retaskable.com, `/<project>/ci/<run>` (the companion list, section 14), is what `target_url` points at.

**What the cloud keeps** (the run record, in the store; all derived by the computer): run id, project, sha, branch, change number, event, who pushed (login), state, which computer and when it took it, and each job's and check's name, state, exit code, duration and a one-line summary the agent cuts from the check's last output line (at most 200 characters). **Never**: the log, the diff, the command line (the command is the project's setting, shown from there), environment or file paths. This is the rule for transcripts (CLAUDE.md of Retaskable.com: "Never store session transcripts in the cloud: they're streamed live only") applied to logs, because a log can print secrets and code.

**What the page shows**:
- The run's state, sha, the change it is for, the computer, the checks as a list with their times.
- **Live**, while the computer is online: the log of the check you open, streamed from the computer through the hub's existing live channel (as a lane's transcript is), tail first, with the scroll back to its start. Nothing is written to the cloud on the way.
- **Finished, computer online**: the same, read from the computer's kept log file by the read-only command `ci_log { run, check, before?, limit? }` (section 14): the computer answers from its disk, like `session_history`.
- **Computer offline**: the record only, with "The log is on <computer>, which is offline. It will show when it is back on." and the check's one-line summary. This is also why the host status description always carries the failing check's name (section 6): the answer to "what failed" is on the host even when the log is not reachable.
- **Re-run** (any member of the project) and, for a run awaiting approval, **Approve this run** (section 11).
- **Waiting**: for a queued run the page says why, from the store's `needs_status` (computer-tags.md section 3.3): "Waiting for a computer with `windows`: jcnash-pc4 is offline", or "No computer is on". This is why the host's description stays generic: the store can say who is online, but the site never posts to the host.

**Logs on the computer**: `.retaskable/ci/<project>/logs/<run>/<n>-<check>.log`, plain text, 10 MB per check (the rest is dropped with a "log truncated" line, the exit code still counts), kept 7 days and at most 500 MB per computer, oldest first. The agent's own fix runs use them: when CI fails on another computer, the fix follow-up needs the log tail (`fixFollowUpWithLogs`, ci-link.md section 3); the computer running the fix asks for it with `ci_log` through the hub, so the log still never rests in the cloud. A project on `retaskable` gives the follow-up the failed checks' name, exit code and last 60 lines.
