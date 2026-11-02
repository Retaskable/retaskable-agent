# The agent as CI — Spec

Status: v0.1, draft (2026-11-01). Section 2's **[Decided]** lines are what the owner asked for; every **[Proposal]** line fills a gap and is the owner's call; section 15 lists the questions that are really open, each with a recommendation. Builds on [pieces.md](pieces.md) (runs, leases, run kinds), [engines.md](engines.md), [agent-settings.md](agent-settings.md) and, in Retaskable.com, [ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md), [github-workflow.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/github-workflow.md) section 4.4, [code-storage.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/code-storage.md) section 3.1, [computer-tags.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/computer-tags.md), [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) sections 6 and 7 and [project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md). It is written to agree with the open spec PRs Retaskable.com #2407 (item branches and a merge queue, `merge-queue.md`) and #2425 (projects that just work, eligible computers and trust levels, `auto-projects.md`). Section 14 is the companion list for Retaskable.com, which this change does not edit; per [README.md](README.md) behaviour is decided there first, so that list is what carries this spec over.

Theme: Chores and CI

Kind: new

## 1. What it is

Today a project needs a CI somewhere else: GitHub Actions, GitLab CI or a Woodpecker the owner runs. Retaskable's own repo uses a Woodpecker server whose `.woodpecker/ci.yaml` grew `seen`, `turn` and `merge` steps to cope with the agent (ci-when-ready.md sections 7 to 9). A new project with no CI gets nothing: the lane's checks gate the change, and the code host shows no checks.

The agent already has everything a CI needs: a computer with the project's toolchain, the project's approved check commands ([agent/src/allowedCommands.ts](https://github.com/Retaskable/Retaskable.com/blob/main/agent/src/allowedCommands.ts) in the TypeScript agent), `git`, the host's CLI, and a way to stream a run live to the site. So **the Retaskable agent is the CI for any project run on Retaskable, and the default.** It runs the project's approved checks on a clean checkout of the exact commit, reports the result to the code host as a commit status for that commit, and the merge waits for it. A project that has a CI of its own keeps it.

Nothing here adds a server, a queue or a database: run records are rows in the shared backlog store (as pieces' runs are), logs stay on the computer, and the code host is told through `gh` or `glab`.

## 2. Decisions

- **[Decided]** (owner, 2026-11-01) The agent is the CI for every project on Retaskable, not only Retaskable's own repo. It is the default; a project may use its own instead.
- **[Decided]** It runs only commands the computer's owner has approved or trusted ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1, and #2425's trust levels). No new way for a command to reach a computer.
- **[Decided]** It runs on a clean checkout of the exact commit, and reports to the host against that commit, so what passes is what is on the host.
- **[Proposal]** **Commit statuses, not check runs.** GitHub lets only a GitHub App create check runs; a person's `gh` login can create commit statuses (`POST /repos/{o}/{r}/statuses/{sha}`, scope `repo:status`, which `gh auth login` already has). GitLab's `POST /projects/:id/statuses/:sha` is the same. A status has a `context`, a state, a description and a `target_url`: all this needs. No GitHub App to install, no server-side host call.
- **[Proposal]** **One context, `retaskable/ci`**, for the whole run, whatever the number of checks or computers. The owner marks that one name required; it never changes with the project's check list.
- **[Proposal]** **Computers pull; the host is polled, not webhooked.** A computer notices a commit that needs a run by polling the host as it already polls pull requests (section 8), and asks the store for the run. The site never reads the repo and takes no webhook (CLAUDE.md of Retaskable.com: no server-side repo reads or code-host actions).
- **[Proposal]** **A run is a run kind, `ci`** (pieces.md section 3.3): claimed with a lease and a beat, no lane and no session. A CI run takes a CI slot, not a lane (section 9).
- **[Proposal]** **Logs stay on the computer** and stream live; the cloud keeps only the run's record (section 7).
- **[Proposal]** **Untrusted code never runs on its own** (section 11).

## 3. The project's setting

A new team setting, `ci` (a team field in `shared/projectSettings.ts` terms, not a command, so a member who may change settings may change it; the command fields it uses stay owner-gated):

| Value | Meaning |
|---|---|
| `retaskable` | The agent is the CI. **The default for a new project.** Merges wait on the `retaskable/ci` status. |
| `own` | The project's own CI (GitHub Actions, GitLab CI, Woodpecker, anything that reports checks to the host). The agent waits on the host's checks exactly as it does today (ci-when-ready.md, `awaiting_ci`). The agent runs no CI. |
| `both` | Both run; the merge waits for `retaskable/ci` and for every other check the host shows. For a project moving from one to the other, or one that keeps its own CI for deploys and wants Retaskable's for checks (section 12). |
| `none` | No CI wait: today's `requireCi` off. The lane's checks gate, as for a repo with no CI. |

**What existing projects get** **[Proposal]**: nothing changes by surprise. When the feature reaches a project, one computer works out a default once, from facts it can read (`ciCoverage` over the base branch's workflow files, and the host's required status checks on the base through the `rules` capability), and sends it as a settings offer (the way a scan's `needs` are offered):

1. The host shows required checks on the base, or `ciCoverage` finds CI configuration that runs for the base: **`own`**.
2. `requireCi` is off and neither of those holds: **`retaskable`** is *suggested*, not applied. The project's board shows "Run this project's checks on your computers as CI?" with **Turn on** and **Not now** (Not now sets `none`, which is what it did before). It is a suggestion because turning it on starts posting a status to every pull request and makes the agent's merges wait for it.
3. A project with `requireCi` on and workflows keeps `own`.

New projects (added after this ships) get `retaskable` with the setup in section 10, unless Setup finds CI configuration on the base, then `own` is suggested instead.

**Both at once** is allowed. The only cost is that every change waits for the slower of the two. It is a setting value, not a mode the agent guesses.

## 4. What runs

**[Proposal]** The project's check commands, from the store's settings at the project's current approved `commandsVersion`, and nothing else.

- The agent builds the run's command list from `checks` (`{ name, command, blocking, timeoutSec }`), the same field the lane runs before review. It never reads a command out of the repo (not a `.woodpecker.yaml`, not a `package.json` script it was not told to call): whatever the commit's files say, the list is the owner's. This is what makes a pull request unable to change what CI runs on its own (section 11).
- **A separate list, `ciChecks`, is optional.** Unset, CI runs `checks`. When set, CI runs it instead, so a project can keep the lane's checks quick (a typecheck and the changed tests, before review) and have CI run the whole suite, a release build, or the checks for another platform. `ciChecks` is a command field (`commandsOf`): changing it moves `commandsVersion` and needs the same owner fresh sign-in and the same per-computer approval or standing trust as `checks`. The lane never runs `ciChecks`; CI never skips a blocking `checks` entry just because the lane ran it.
- **Each entry may carry `needs`**, tags from computer-tags.md (`windows`, `mac`, `linux`, `gpu`, `display`, `docker`). A check needing `windows` runs only on a computer with that tag; one entry per platform is how a project gets a matrix. It adds nothing to the project's own `needs`.
- **Environment the command sees**: the computer's normal environment minus the agent's own secrets (the cloud and store tokens, `GH_TOKEN` and `GITHUB_TOKEN`, `GITLAB_TOKEN`, the OS credential store is not exposed), plus `CI=true`, `RETASKABLE_CI=1`, `RETASKABLE_CI_EVENT` (`pull_request`, `push` or `queue`), `RETASKABLE_HEAD_SHA` and `RETASKABLE_BASE_SHA` (the merge base of the head with the base branch, absent for a push). A project may use the last two to run only the tests a change can affect, as `ci.yaml` does today (ci-when-ready.md section 6).
- **A project with no approved commands** (`commandsVersion` not approved on any eligible computer, #2425 section 3: "Commands not set up yet") posts `error` with the description "No checks are set up for this project", never `success`: a CI that ran nothing does not pass.
- **Non-blocking checks** run, are shown on the run page and cannot fail the run. Their failure is in the description ("passed; 1 non-blocking check failed: lint").
- Each check keeps its `timeoutSec` (default per the lane's rule); the whole run has a ceiling, `ciMaxMinutes` (default 60, a computer-independent team setting). A timeout is `error` with the check named.

## 5. Where it runs: a clean checkout

**[Proposal]** The point of CI is to catch what is only true on the agent's disk: a file never committed, an ignored file the build leans on, a dependency left installed, a generated file. So:

1. **A mirror, not the project's checkout.** Per computer and project, the agent keeps a bare clone under `.retaskable/ci/<project>/mirror.git` (made on the first run with the host's CLI, never in the person's own checkout or the lanes). It fetches the commit by id (`git fetch origin <sha>`; a pull request's head from `refs/pull/<n>/head` on GitHub or `refs/merge-requests/<n>/head` on GitLab, which is how a fork's commit arrives without adding the fork as a remote).
2. **A fresh detached worktree per run**, `git worktree add --detach .retaskable/ci/<project>/runs/<run>/work <sha>`: no session files, no `.retaskable` folder, no untracked or ignored leftovers, because it did not exist a moment ago. Submodules are initialised at the commits the tree records (`git submodule update --init --recursive`). Git LFS files are pulled if the repo uses them.
3. **Checked before it starts**: `git rev-parse HEAD` equals the run's sha and `git status --porcelain --ignored` is empty. If not, the run ends `error` ("the checkout isn't clean"), never a pass.
4. **Removed after the run**, pass or fail, with its output (`runs/<run>/`), except the logs (section 7), kept for 7 days and a size cap. A leftover `runs/` folder on start (a restart) is removed.
5. **Not touched by the checks afterwards**: the agent notes whether `git status --porcelain` is still clean when the checks end. A check that rewrote tracked files is shown ("lint changed 3 files") but is not a failure by itself: a formatter is allowed to be `--check` or not, and the owner's command is the owner's.

**Dependency caches are allowed and are the speed win; build outputs never leak into the checkout.**

- A cache is a folder outside the worktree, `.retaskable/ci/<project>/cache/<kind>/<key>`, where **kind** is a package manager's download store (npm's `npm_config_cache`, pnpm's store, Cargo's registry and git folders via `CARGO_HOME`, pip's cache dir, Go's module cache `GOMODCACHE`, Gradle's `GRADLE_USER_HOME`, NuGet's packages folder) and **key** is the hash of the lockfile(s) the agent finds in the tree (`package-lock.json`, `pnpm-lock.yaml`, `Cargo.lock`, `requirements*.txt`/`poetry.lock`, `go.sum`, `gradle.lockfile`, `packages.lock.json`) plus the OS, architecture and, for Cargo, the toolchain file if any. The agent points the tool at the folder with the tool's own environment variable; it copies nothing into the worktree.
- A key that has no folder starts from the most recent folder of the same kind (a download store is content-addressed, so a slightly stale one is safe), copied, then used and kept under the new key. At most 3 keys per kind and project are kept, and the folder total is capped (default 5 GB, an agent setting); the oldest go first.
- **Never cached or carried between runs**: `node_modules`, `target/`, `dist/`, `build/`, `.next`, `__pycache__`, and anything else the worktree created. They are in the worktree, which is deleted. A project that wants a build cache (Rust's `target`, a CMake build folder) sets it outside the tree with its own variable in a check command; Retaskable does not do it for it, and section 15 asks whether it should.
- A cache is per computer, never uploaded.

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

## 9. Which computer, and the slot

**Eligible**: a computer eligible for the project (#2425 section 4.1: the owner is in the organisation, the trust level covers the project, its login reaches the repo, it has not skipped the project), whose tags include the project's `needs` and the job's `needs`, with a free CI slot.

**[Proposal] CI has its own slot, `ciSlots`** (a computer setting, default 1, 0 turns CI off on that computer), **not a lane.** Why:
- A lane is a worktree, a session, a model and a spend (agent.md section 4.1). A CI run needs none of the three, so charging it a lane would idle a session's capacity for a process that costs CPU only, and would let a long session block CI that a review is waiting on.
- CI is on the critical path to every merge. If it shared lanes, a computer full of build sessions would hold the checks of the changes it just built.
- The cost is CPU and memory contention with the lanes on the same computer. That is the owner's to size (`ciSlots`, and nothing else in v1); the run starts with the lowest priority the OS lets a child take (nice 10 / below normal), so a session's checks win a contested CPU.

**Choosing the computer**: pull, as for pieces. A computer with a free slot asks the store for a CI job (`ci_take`), which hands out the oldest queued job it can take (needs matched), event priority first: the agent's own ready change and a staging commit before a base push before a person's pull request. **Warm affinity**: the store prefers a computer that ran this project's last CI (its caches are warm): other computers wait a short grace (20 seconds) before a job whose last computer is online with a free slot. This is a preference, never a wait: if that computer is busy or gone, the grace ends.

**The same computer as the change?** Allowed, and the common case with one computer. A change is not exempt from a clean-checkout run because the same computer built it; the mirror and worktree make it as clean as any.

**No computer on**: the status stays `pending`, "Queued: waiting for a computer to run the checks" (section 6). The agent's own merge for that change waits, as it waits on a host CI that has not started; its `awaiting_ci` 10-minute "no checks showed up" grace does not apply to `retaskable` (the store knows the run exists) and is replaced by the 2-hour pending limit, which on expiry asks a person ("CI has been queued 2 hours: no computer is taking it") rather than merging unchecked.

**Waiting on the project's `needs` does not fail**: the run waits (like items do, computer-tags.md section 2: "Waiting for a computer that has it").

## 10. The gate

**The agent's own merges** wait on `retaskable/ci` at the head, itself: `awaiting_ci` reads the commit's checks (`CodeHost.checks` / `commitChecks`), which now includes the status, so for a `retaskable` project that path is unchanged and works whether or not the host enforces anything. The pending, failure and error handling there (fix runs, `ciHealth` machine failures, "CI failed on this commit too" in a review send-back) apply as they do now. With `ci` of `retaskable` or `both`, the agent never merges a change whose head has no `success` for `retaskable/ci`.

**Setup tells the owner to make the check required** on the base branch:
- GitHub: Settings, Branches (or Rulesets), "Require status checks to pass", add `retaskable/ci`. A repo on a free plan that is private has no branch protection at all (GitHub only offers it on paid plans or public repos); Setup says so.
- GitLab: Settings, Merge requests, "Pipelines must succeed" does not apply to external statuses on the free tier; Setup shows the external status check setup where the plan has it, and otherwise the same note.
- The check reads the base branch's protection through the `rules` capability. The project's board shows "`retaskable/ci` isn't a required check" with the steps, until it is.

**What happens if they don't**: the agent's own merges are still gated (above). What is not gated is a person's merge on the host's button, which GitHub allows over a red status when nothing is required. The status is still red on the pull request, the site's own **Merge** (`merge_pr`) button is greyed with "CI hasn't passed", and the agent's `merge_pr` handler checks the status again and refuses a red or pending head (a person may override with a fresh sign-in, and the override is noted in the item's history). So the unprotected case degrades to "visible and discouraged", not "unchecked".

**`both`**: the merge waits for `retaskable/ci` and for every other check on the head, reading the host's checks as it does now. The project's own checks may also be required by the host as before.

## 11. Untrusted code: threat model

CI runs project code on a person's computer. For a change the project's own members wrote that is what a lane already does. For a change from outside it is a stranger's code running with the owner's rights. So:

**[Decided]** (owner, 2026-11-01) A pull request from a fork, or by a person outside the project's organisation, never runs automatically; a project owner approves the run.

**[Proposal] Who is trusted**, worked out by the computer from what the host says, never from the site: the change is trusted when its head is in the same repo as its base (`isCrossRepository` false, GitLab: source project equals target project) **and** the author's association with the repo is `OWNER`, `MEMBER` or `COLLABORATOR` (GitLab: Developer or above; or the agent's own login). Anything else is **untrusted**. A branch pushed by a collaborator but containing a fork's work merged in is still the collaborator's (the author is who pushed, and a review is the approver's check).

**[Proposal] The approval gate** has two keys, both needed, and both can be refused independently:
1. **A project owner approves the run** on the run page (**Approve this run**), freshly signed in (10 minutes, like `approve_commands`). It names a sha: a new push to the pull request is a new run and a new approval. The store records approver and sha; the hub answers `ci_approve` itself from the store, as for other store-only commands, so there is no new thing for an agent to be told to do.
2. **The computer allows untrusted runs.** A computer setting, `ciUntrusted` (off), set only at the computer in its own settings page like network access or tags (agent-settings.md section 2.1: nothing on the site changes what a computer is pointed at or allowed to run). A computer with it off never takes an approved untrusted run, so a compromised site cannot make any computer run a stranger's code by approving it.
3. Recommended: set `ciUntrusted` only on a computer in a container (tag `docker`). **[Proposal]** An approved untrusted run is only handed to a computer with the `docker` tag in v1, whatever `ciUntrusted` says, because the agent in Docker (docker.md) cannot touch the owner's own files, keys or checkout. A project without such a computer sees the run wait with "Needs a computer in a container (`docker`) to run code from outside the project".

**What the untrusted code can and cannot change**, even when approved:
- **Not the commands.** They come from the store's settings (section 4), not from a file in the pull request, so a pull request cannot add a step, change a flag or skip a check. What it can change is what those commands do with the code it ships (a test, a build script, a lint config, a `package.json` script the check calls). That is the real exposure, and why the approval exists.
- **Not the secrets.** The run's environment drops the agent's secrets (section 4). The container has no credentials, and `GH_TOKEN` is not passed.
- **Not the status.** A check cannot write a status: the agent posts it from the exit codes; the check's own output cannot forge `success`.
- **Not other projects.** The run's worktree and caches are per project. For an untrusted run the cache is read-only and starts from a trusted cache copy, and its writes are discarded, so a fork cannot poison the cache a later trusted run reads (cache poisoning).
- **Network**: untrusted runs have no network after the dependencies are fetched when the platform lets the container do it; when it can't, the run is shown as "network on" next to the approval.

**Other threats**

| Threat | What stops it |
|---|---|
| A compromised retaskable.com makes a computer run a command it was never approved for | Section 4: the commands are the owner-approved or trusted ones, checked by the computer itself; no `ci_*` command carries a command line; a new `ci_request` or `ci_approve` carries a sha and a run id. A site can request a run, not change what runs |
| A compromised site approves an untrusted run | The computer's own `ciUntrusted` is off unless set at the computer, and only a `docker`-tagged computer takes it |
| A pull request author changes the checks | They come from settings, not the repo (section 4) |
| A malicious commit already in the project's own branches (a member's account taken over) | Same as for a lane today: trusted authors' code runs. The standing trust level (#2425 section 5) says whose commands, not whose code |
| A check posting a fake `success` or a status for another context | The agent posts statuses from outside the checked code; the checks have no token |
| A log leaks a secret | Logs never go to the cloud (section 7); they stream to a signed-in member of the project, are kept on the computer for 7 days and are cut at 10 MB |
| Resource abuse by a stranger (a fork pushing 500 commits) | Untrusted runs need an approval per sha, and a project owner can **Stop approving** (a per-project setting that makes the site refuse `ci_approve` for the project) |
| A run reads the owner's own files | A trusted run is the same as a lane: it can. That is why the `docker` computer is the recommendation for anything else. A trusted run has no more reach than the lane's checks do |
| A cache poisoned by one run to hurt the next | Section 5: keys by lockfile hash; downloads are content-addressed by their package manager; untrusted writes are discarded |

CLAUDE.md of Retaskable.com says "the website never runs arbitrary code on an agent: a project's commands change only by an owner who has just signed in, and run on a computer only once that computer's owner approves them on the site." That sentence stays true; this adds CI as one more place those commands run, and adds no command that carries a command line.

## 12. Interplay

**With ci-when-ready ([ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md))**: `ciWhenReady` and the `ci-ready` label exist to keep a host CI from running before a change is ready. With `ci` of `retaskable` the agent decides when to ask for a run, so neither is needed: "ready" (pushed, section 5 of that spec) requests the run. The label stays for `own` and `both`. `ciCoversChecks` has a counterpart: for a `retaskable` project a team setting, `ciCoversChecks`, may skip the lane's pre-review checks that CI runs beside the review (the same trade as before). It stays off by default.

**With the merge queue (#2407)**
- **Item pull request**: CI on the item's pull request to the base is a `retaskable/ci` run on its head (the agent's own change ready). A piece's pull request into the item branch has none (section 8).
- **The staging branch.** #2407 section 11 has Trigger A (a push to `merge-queue/<base>` that the project's own CI is configured to run on) and Trigger B (a draft pull request). With `retaskable` CI, there is no trigger to configure: the holder pushes the branch and asks for a run on that commit (event `queue`), and reads the `retaskable/ci` status at that sha with the same `commitChecks` call it uses for any CI. "CI doesn't run on pushes here: Needs setup" (its section 13) cannot arise for a `retaskable` project. For `both`, the holder waits for the staging commit's other checks as it does for any CI.
- **Cancelling stale staging runs**: its `ciRuns.cancel(branch, exceptSha)` for Retaskable CI is a store op that ends the run `cancelled`; the computer holding it kills the process tree. No CI API token, no per-CI code.
- **Reds**: the bisect works unchanged on the status. A red whose cause is a machine failure (`ciHealth`) reruns the same commit; here that is `ci_rerun`, free.
- **Offline holder, restart**: the queue's rule that the next holder finds the staging commit's result and continues applies: the run's result is in the store, the status is on the host.
- **Combination tests and the lone entry**: a lone queued entry that "merges as it is" (its section 10.1) has already had its own `retaskable/ci` run on its head; there is no staging run to wait for.

**With restart ([agent.md](agent.md) section 7.4, `restart-cuts-sessions`)**: Section 13.

**With `own` and `both` projects' deploys.** Out of scope, section 16.

**With the existing Woodpecker for Retaskable's own repos**: they stay on `own` until the queue ships, because `seen`, `turn` and `merge` are Woodpecker-specific coordination that the queue replaces. Moving them to `retaskable` is a later step, and not part of this spec's PRs.

## 13. Restarts, going offline, and what is never reported as a pass

- **A CI run is a run**: it has a lease and a beat to the store (pieces.md section 3.3). A restart kills the run's whole process tree with every other session (`restart-cuts-sessions`), the run ends `interrupted` and its job returns to the queue in the store. A computer going away is the same after `RUN_STALE_MS` (3 minutes without a beat), when the store ends it `lost`.
- **Never `success`**: a run that did not finish every blocking check on the exact sha cannot post `success`. The status for an interrupted run is left `pending`, with its description updated by the computer that restarts (on start the agent lists its own unfinished runs, posts "Interrupted: running again" for each, and ends them) or, if that computer does not come back, by the next one to take the job. A restart never posts `failure` for the cut run either: it is a machine event, not the code's.
- **Rerun**: the requeued job is taken by any eligible computer (the warm-affinity grace applies for the one that restarted). A job interrupted three times is `error` ("Cut by a restart three times"), and the run page says so, so a restart loop cannot hang a merge forever.
- **Order of checks after a restart**: none is resumed; the job starts again from the clean worktree. A check is not assumed idempotent.
- **A pass on one computer is a pass on that sha**: if the commit changes (a push), the old run is superseded (section 8).
- **A status the agent cannot read back**: the agent's merge treats a `pending` or missing `retaskable/ci` as not passed.
- **The store being unreachable** fails closed (README: every project has a shared backlog and the agent fails closed): no run is claimed, and no status is posted from a run that cannot record its result.

## 14. Companion changes in Retaskable.com

Not made here; each is for the Retaskable.com repo, where this behaviour is decided first. In an order that suits the PR plan:

1. **Spec words**: a new `docs/specs/agent-ci.md` there (this file's source), plus edits to ci-when-ready.md (section 1: "the repo's CI" is a project's choice; sections 2 to 5: `ciWhenReady` and the label apply to `own` and `both`), github-workflow.md section 4.4 (the gate reads `retaskable/ci`), code-storage.md section 3.1 (the `commitStatus` capability, with `checks`/`commitChecks` reading it), computer-tags.md (a job's `needs`), agent-settings.md (`ciSlots`, `ciUntrusted`, cache size), pieces.md (run kind `ci`), merge-queue.md section 11 (the staging branch needs no trigger for `retaskable` CI) and cloud/07-threat-model.md (section 11's rows above).
2. **The setting** `ci` (`retaskable`, `own`, `both`, `none`) as a team field in `shared/projectSettings.ts` with the schema, and `ciChecks` (a list of the same shape as `checks`, plus optional `needs`) as a command field in `COMMAND_LABELS` and `commandsOf`, so changing it moves `commandsVersion` and needs the owner's fresh sign-in and the computers' approval. Other team fields: `failFast`, `ciMaxMinutes`, `stopApprovingUntrusted`. Setup's CI section, with the required-check instructions (section 10) and the migration suggestion (section 3).
3. **Store ops** (no repo access, derived data only): `ci_request { project, sha, event, change?, branch, author, trusted }` (idempotent on project+sha+event), `ci_take { agent }`, `ci_beat`, `ci_job_finish { job, results }` (answers "last job?"), `ci_cancel { project, branch?, exceptSha? }`, `ci_rerun { run }`, `ci_approve { run }` (the owner, fresh sign-in; refuses when `stopApprovingUntrusted`), and a read `ci_runs { project, sha? , change? }`. The run record keeps no log, command or path (section 7).
4. **The run page** `/<project>/ci/<run>` and a CI row on an item's or pull request's card linking to it; the board's "queued" reasons from `needs_status`.
5. **Commands** in cloud.md section 6.1: `ci_log` (read-only, `{ run, check, before?, limit? }`, answered from the computer's own log file, relayed and never stored; like `session_history`) and `ci_rerun` and `ci_approve` as the hub's own answers from the store, `issuedBy` recorded. None carries a command line or a path. Live log streaming reuses the transcript channel. `merge_pr` refusing a red or pending `retaskable/ci` (section 10).
6. **The threat model**: the rows of section 11, in cloud/07-threat-model.md.
7. **Dogfooding**: nothing; Retaskable.com stays on `own` (section 12).

## 15. Open questions

1. **Should the lane's quick checks and CI be one list or two?** Recommended: two, with `ciChecks` unset meaning "the same as `checks`" (section 4). A project that never sets it gets the clean-checkout confirmation of what the lane ran, at the cost of running the checks twice; the owner can then trim the lane's list, or turn on `ciCoversChecks`.
2. **Do existing projects with no CI move to `retaskable` by themselves?** Recommended: no, suggest it (section 3). Starting to post a status and gate merges is a behaviour change.
3. **Statuses or check runs?** Recommended: statuses (section 2), because check runs need a GitHub App. Revisit if a GitHub App is wanted anyway: it would give annotations and a richer pull request panel, but means every project installs it and the site holds a key, against "server never touches code".
4. **Webhooks or polling?** Recommended: polling in v1 (up to a minute's latency, section 8). A webhook would deliver only a sha, branch and author, but is a new inbound path into the cloud, and the cloud would then have to hold a host credential to verify it. Decide after measuring the latency.
5. **Should build outputs be cacheable (Rust's `target`, a CMake build)?** Recommended: not in v1 (section 5). Retaskable's own Woodpecker config records how a shared target dir went wrong (a corrupt incremental cache segfaulting every later run, `ci.yaml`). Add later as an explicit, per-project, lockfile-and-toolchain-keyed cache, wiped on any `error` run.
6. **Does an untrusted run require a container (`docker` tag)?** Recommended: yes in v1 (section 11). It makes the approval safe by construction, and a project with no such computer gets a clear wait. Relaxing it is a computer-owner decision (`ciUntrusted` alone) if they accept the risk.
7. **Is the status description enough when the computer is offline?** Recommended: yes, with the failing check's name and exit code on the host and the one-line summary per check on the page (sections 6 and 7). Caching a failure tail in the cloud is refused: logs can carry secrets and code.
8. **Run the merge result or the head?** Recommended: the head (section 8). The queue (#2407) is what tests the combination; a pull request-level merge run would double that.
9. **Priority when a computer has both a session and a CI job.** Recommended: independent slots with CI at lower OS priority (section 9). If it hurts in practice, the answer is `ciSlots`, not a scheduler.
10. **A person's `automerge` label (Woodpecker's today)** has no counterpart on a `retaskable` project. Recommended: the agent merges a person's labelled pull request once `retaskable/ci` is green, behind the same merge turn and lock as its own (github-workflow.md section 4.4). Not in these PRs; the queue spec decides it (#2407 section 10.2: the owner's `automerge` stays outside the queue "for now").

## 16. Not in this step

- **Deploys and releases.** They stay with the project's own pipeline or Retaskable's deploy features (staging deploy, promote, rollout). A project that wants deploys from its own CI while Retaskable runs the checks sets `ci` to `both`, or to `retaskable` and keeps its deploy workflow filtered to push events on the base (a deploy job should depend on its own checks, not on `retaskable/ci`, or be triggered by a push to the base that has the status `success`). Deploys already wait on the base's CI being green (the fix-main rule); that now reads `retaskable/ci` as it reads any check.
- **Scheduled and nightly runs**, matrix expansion beyond per-entry `needs`, artifacts and test reports uploaded anywhere, secrets for checks (a check that needs a secret reads it from the computer's own environment, which the owner set; the cloud never holds one).
- **Windows-only and macOS-only toolchains** are handled by tags, not by the agent installing anything.
- **Replacing the lane's checks.** The lane keeps running its checks before review.
- **Running CI for a project nobody has a computer for.** There is no hosted runner.

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
