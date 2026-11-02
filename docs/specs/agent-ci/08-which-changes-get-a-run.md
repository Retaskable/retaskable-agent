## 8. Which changes get a run

A run is for a **commit** (a sha) and an **event**. The store makes one run per `(project, sha, event kind)` and ignores a second request (idempotent), so several computers noticing the same commit make one run.

| Event | Trigger | Notes |
|---|---|---|
| The agent's own change is ready | The agent requests it when the change is pushed and goes to its checks (ci-when-ready.md section 5: "ready is pushed"), replacing the `ci-ready` label for a `retaskable` project | CI runs beside the lane's checks and the review, and the merge still waits for all three on one commit |
| A fix push, a conflict-merge push | The agent requests a run on the new head | As today, a fix runs CI again |
| A push to the base branch (after a merge or a person's push) | A computer polling the base's head sees a sha with no `retaskable/ci` status and no run | So two changes that passed apart and broke main show up. Red hands the project's fix-main chore (`fix_main-<base>`, `mainCi.ts`) the failing run, as for any CI |
| A merge-queue staging branch (#2407) | The holder pushes `merge-queue/<base>`; it requests a run on that commit | Section 12 |
| A pull request written by a person, from the project's own repo, by a member | A computer polling `gh pr list` / `glab mr list` sees a new head sha | Runs automatically if the author is trusted (section 11) |
| A pull request from a fork, or by someone outside the organisation | Same polling | **Does not run until approved** (section 11) |
| A piece's pull request into an item branch (#2407) | Never | A pull request whose base is a `retaskable/<id>-item` branch is not CI'd; the item's pull request to the base is |
| A pull request marked draft | Never | Runs when it is marked ready |
| A spec-only draft (agent.md "a spec draft never gets CI") | Never | As today |

- **Polling**: each eligible computer, on its normal tick, lists the project's open pull requests and the base's head (one `gh` or `glab` call each, with `--json` fields `headRefOid`, `isCrossRepository`, `author`, `authorAssociation`, `isDraft`, `baseRefName`), and for any sha without a status named `retaskable/ci` asks the store for a run (`ci_request`) and posts the first `pending` (next bullet). To keep the polls cheap only projects with `ci` of `retaskable` or `both` and an open pull request or a moved base are polled, and a computer asks at most once a minute.
- **The first `pending`** is posted by the computer that asked for the run, at once, so the pull request shows "Queued" within a minute even if no computer then takes it. (If it then goes offline the status stays `pending`; the run page, not the host, says why.)
- **Latency**: a person's pull request starts CI up to a minute after it opens. Webhooks would cut it; that is a decision about a new inbound path (section 15) and v1 doesn't need it.
- **Superseded**: a new head on the same pull request cancels the older head's run (queued: dropped; running: the computer kills its process tree and the status is left as it was, since the newer head has its own).
- **A run is never requested for a sha that is not reachable on the host**: the poll returns only commits the host has.
