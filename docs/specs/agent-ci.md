# Agent-run CI — Spec

Status: v0.1, a draft, backlog id `agent-ci`. Builds on [agent.md](agent.md) sections 4.1 and 5 (`awaiting_ci`), [agent-settings.md](agent-settings.md) section 3, [agent-telemetry.md](agent-telemetry.md) section 3, [pieces.md](pieces.md) section 3.2 (`watch`) and [docker.md](docker.md). Copies, for the Rust agent, what Retaskable.com's [ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md) section 10 says about CI run on the agent computers (since 2026-11-02, off Woodpecker).

Theme: Orchestration

Kind: new

## 1. What it is

In Retaskable.com, `.agent-ci/runner.mjs` polls GitHub and checks each ready pull request, merged with current main, in a Docker container at low priority. It posts the commit statuses the orchestrator waits on, comments the tail of a failed log on the pull request, squash-merges `automerge` pull requests on green, and holds a person's merge while an agent pull request that is up to date with main is still being checked. This spec has the Rust agent do the same, on its own slot, so a computer whose every lane is busy building still runs the CI those lanes wait on.

**CI is its own binary.** It is not a task inside the agent: `retaskable-ci` (`src/bin/ci.rs`) is a separate process with its own link to the backlog store, shaped like the Chat Agent's ([chat-agent-process.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/chat-agent-process.md)); Retaskable.com's [ci-agent-process.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-agent-process.md) is the contract. The agent (`src/main.rs`) starts no CI loop, reads no CI settings and shows no CI on the Now tab or in the tick. Sections 3.1 to 3.6 describe what the loop does; section 3.7 is how the process gets its repos, its link and its settings.

The Rust agent's own repo already carries the project side of it: `.agent-ci/image` (one line, the Docker image: `rust:1-bookworm`) and `.agent-ci/checks.sh` (run as `bash .agent-ci/checks.sh <pull_request|push>`, the same checks as the repo's workflow).

## 2. Decisions

- **[Decided]** CI is its own process, `retaskable-ci`, with its own slots (`AGENT_CI_SLOTS`), never a lane and never a session slot. It doesn't count toward `lanes` or `concurrency`, and shares nothing with the agent but the computer.
- **[Decided]** It runs at low CPU priority.
- **[Decided]** The code host token never shares a container with the code under test. Only a separate fetch step, on the host, sees it.
- **[Decided]** Several computers may run CI for one repo; they never run the same commit twice, and claim a commit with a pending status.
- **[Decided]** A failure reaches the fix session as a comment with the last 120 lines of the failed log.
- **[Decided]** Windows, Linux and macOS are all supported, and Docker is required.
- **[Proposal]** Run the loop natively in Rust rather than supervising the project's `runner.mjs`. Reasons: `runner.mjs` is Retaskable.com's own file (this repo has only `image` and `checks.sh`, so there is nothing to supervise here); supervising it needs Node on every computer, which a Windows install of this agent doesn't have; and a child process the agent can't see into is harder to stop on restart (agent.md section 7.4) and to hold to the token rule below. The contract stays the project's: its `.agent-ci/image` and `.agent-ci/checks.sh`, so a project's CI is the same whichever agent runs it. A project that has only `runner.mjs` and no `image` gets no agent CI from this agent (section 3.1).
- **[Decided]** `AGENT_CI_SLOTS` defaults to 2, a whole number; 0 runs nothing. `AGENT_CI_CPUS` (default 8, capped at the CPUs Docker reports) is each check's `--cpus`.
- **[Proposal]** A claim is stale 15 minutes after its last refresh (section 3.4).
- **[Proposal]** Where this spec says the statuses' names and wording are "the TypeScript runner's", the build copies them from `.agent-ci/runner.mjs` in Retaskable.com rather than inventing new ones, so the orchestrator's wait is unchanged.

## 3. Behavior

### 3.1 Discovery and the slot

- **Discovery.** For each repo in `AGENT_CI_REPO` (GitHub, `gh`; section 3.7), the loop looks in the repo's own mirror's `origin/<baseBranch>` for `.agent-ci/image` and `.agent-ci/checks.sh`. Both present: the project has agent CI. One missing: it doesn't, and the loop says why once in its log. It reads them from main, not from a pull request, so a pull request can't change what runs its own CI.
- **The slot.** `src/agent_ci/` holds the loop (`mod.rs`), the claims (`claim.rs`), the container run (`container.rs`), the fetch step (`host.rs`) and the code host side, statuses and the failure comment (`report.rs`). It is the whole of `retaskable-ci run`, with `AGENT_CI_SLOTS` permits. A permit is held from the claim to the last status posted. Lanes, sessions and `concurrency` neither take nor wait for one.
- **As built.** `discovery.rs` (the `git show` of the two files and the fetch of `origin/<baseBranch>`) and `prs.rs` (`gh pr list` and the check keys) sit beside `mod.rs`. A poll probes Docker again each time, so a Docker started later resumes without a restart; `AGENT_CI_SLOTS` 0 probes nothing and says so. There is no backlog-loop gate: the loop depends on no orchestrator, so it runs whether or not an agent is up. The reason a repo has no CI is logged once (again if it changes). There is no Now-tab header and no `ci` in the tick: what the Now tab showed is log lines now (`checking <pr or push>`, `passed in <n>s`, `failed in <n>s`). Only GitHub (`gh`) is read yet.
- **As built, one check.** After each poll `CiLoop::start` takes a permit (`try_acquire_owned`; none free waits for the next poll) for each key this loop hasn't started, and `check.rs` runs it with the permit held to the last status: pending (`agent-ci <computer>: running`) on the pull request's head, fetch of `refs/pull/<n>/head` into main (`host.rs`), a conflict (or a failed fetch) fails the check with that reason, the failure comment and no container, else the container run (`container.rs`), then success or failure (a non-zero exit, the ceiling or an error) and on failure the comment; the directory is deleted. A started key is not run again by this loop. `started` and `finished` `ci` events (written under `<data>/ci/projects/<owner>-<repo>/events`) carry `item` (`PR #<n>`), `key` (`<head7>-<main7>`), `pr` and `ms`. At start, `recover` removes the container named in each leftover `<dir>.name` file with `docker rm -f` and sweeps the leftover directories.
- **Low priority.** The loop's children (`git`, `docker`) start at below-normal priority, as the agent's own sessions do (agent.md section 4), and the container gets `--cpu-shares 128` so it yields to the computer's own apps and to the agent's sessions.
- **What is checked.** Each poll (every `pollIntervalSec`) lists the project's open pull requests that are ready (`ci-ready` label when `ciWhenReady` is on, as in agent.md section 5; otherwise every non-draft one) and, per pull request, the head sha and current main's sha. The commit to check is the pull request's head merged with main. The key of a check is the pair (head sha, main sha); a new main sha makes it a new check. A pull request that conflicts with main fails the check with that as its reason, without a container.
- **A person's merge.** While an agent pull request that is up to date with main is queued or being checked, a person's pull request waits: the loop leaves its merge-hold status pending (the TypeScript runner's) until the agent's is checked, then releases it.

### 3.2 One check, and the token

1. **Fetch (host, token).** In a fresh directory under the project's data folder (`std::path`, never a hard-coded separator), clone from the repo's existing object store with `git`, fetch the head and main using the code host's own credentials, merge head into main locally and detach. The directory's `.git` is then rewritten to carry no remote URL, no credential helper and no token: the fetch step's own credentials never land in it.
2. **Run (container, no token).** `docker run --rm` with the directory mounted as the working directory, the project's `image`, `--cpu-shares 128`, a wall-clock ceiling of 45 minutes (**[Proposal]**), no host mounts beyond that directory, none of the agent's environment (the allowlist is `CI=true` and what `checks.sh` is told: `pull_request` or `push`), and `bash .agent-ci/checks.sh <pull_request|push>`. The container has no `gh`, `glab`, token, SSH agent or Docker socket. It may reach the network, since checks install their own tools.
3. **Report (host, token).** The exit code and the log's tail go back through the code host's CLI, never from the container (section 3.5).

The directory is deleted when the check ends, on a restart's recovery, and for any left from before at start; a container still running from a cut check is removed by its name (`retaskable-ci-<repo>-<short sha>`).

A push to main runs the same steps with `push`, on main alone, for the baseline the orchestrator reads (agent.md section 6.7, fix main). As built, each poll lists main's check beside the pull requests' (`CheckKey::push`, pr 0, key = main's short sha alone). It is claimed, held under a permit and reported like any key, but under the `ci/agent/push/ci` context on main's commit, with `checks.sh push` on a fetch of main alone, and a failure posts no comment (there is no pull request). A main sha already run (here or on another computer) isn't run again; a new sha is a new run.

### 3.3 Statuses, merging and the hold

The loop posts the commit statuses the orchestrator waits on, with the TypeScript runner's contexts and descriptions (`agent-ci <computer>: running`, `passed in Nm`, `failed in Nm <tail>`, cut to 140 characters): pending when claimed, success or failure when done. A green pull request labelled `automerge`, or one a person asked to merge on the site (the store's `ci_merge_requests`), is squash-merged by the loop, pinned to the checked head (GitHub refuses if the branch moved). There is no store lock: the merge queue is GitHub's. **The hold** (`automerge.rs`): a pull request whose branch isn't `retaskable/` waits, by not merging yet, while an agent pull request that is open, ready, 0 behind main (`compare`; an unreadable answer is not up to date) and has a pending `ci/agent/pr/ci` status is being checked, for at most 25 minutes from the first wait. There is no `ci/agent/turn` status. A host that can't be read holds no one.

- **As built.** `automerge.rs`: `Automerge::run` reads the pull request (`GET pulls/<n>`); only an open one at the checked head, labelled `automerge` or asked for in the store, goes on. Then the hold above, then `PUT pulls/<n>/merge` with `merge_method=squash` and `sha=<head>`, then one try of `ci_merged {sha, context: ci/agent/pr/ci, pr}`. A failed merge is logged and tells the store nothing. A green check calls it, and so does each poll for every ready pull request that asks and whose newest `ci/agent/pr/ci` status is success (a late label, a person's Merge, a wait that ended). Each poll also reads `ci_merge_requests` and, once per closed pull request whose head repo is this one, sends `ci_pr_closed {pr, merged}` (told again if the store refused). Only GitHub is merged yet.

### 3.4 Several computers, one repo

The store's lease is the claim, as in `runner.mjs`: `ci_claim {sha, context, event, pr?, branch?, title?, author?, computer}` answers won, held (with the holder) or finished, and the check runs only on won. A held key is looked at again next poll; a finished one is done. A commit whose newest status of the context is another CI's success, or an `agent-ci` status that isn't pending, isn't claimed at all (`wants_run`, a port of `claim.mjs`). A running check sends `ci_heartbeat {sha, context}` every 5 minutes, posting its pending status again then; when it ends it sends `ci_finish {sha, context, event, pr, result, minutes, computer, branch?, title?, author?}` (3 tries, 5 s apart). `once <sha>` claims with `force`. All ops go to `<store>/v1/repos/<GitHub repo id>/ops` as `{op, args}` with the binary's own `ci` token; with the store unreachable or unlinked nothing new runs.

**As built.** `store.rs` is the ops (`CiStore`, `StoreOps`: one retry on 401 with a fresh token) and the claim's wire shapes; `claim.rs` is `wants_run`, `newest` and `refreshing`. The old status-based claim (`decide`, key suffixes in descriptions, `stale_taken_over`) is gone. A loser forgets the key, so the next poll looks again; a host or store that can't be read runs nothing and also looks again. Deviations from `runner.mjs`: minutes in descriptions are whole, the poll is 10 s not 30 s, and the status links to no log until a log is kept.

### 3.5 Failures reach the fix session

A failed check comments the last 120 lines of the log on the pull request, once per check key, through the code host's CLI, headed by the check's name and short sha and marked so the loop can find and replace its own comment. The fix session reads the pull request's comments as it already does for a red CI (agent.md section 5, `awaiting_ci`: a failure goes back to `implementing` with the failed checks' names and links), so nothing in the orchestrator changes. The full log is kept on this computer for 7 days, and the status links to it only when the computer is reachable; the comment is what survives.

What the computer reports:

- **Events.** The `ci` event (agent-telemetry.md section 3) gains `claimed`, `started`, `finished` and `stale_taken_over` states, with the item or pull request, the key and `ms`.
- **To the backlog.** Nothing about a check is sent by this slice: the log and code stay here (README.md's rule: the server never sees the user's code). The statuses on the code host are what the store's own CI reading uses. The store lease and history ops are a later slice.
- **Settings page.** None: the agent's settings page, tick and `MACHINE_KEYS` no longer know CI (`ciSlots` and the Now-tab CI header are gone). Slots and CPUs are the process's environment (section 3.7).

### 3.6 Platforms and Docker

- **Docker is required.** The loop uses the `docker` CLI on PATH. On Windows that's Docker Desktop in Linux-containers mode (the image is Linux, as the project's `checks.sh` is bash); on macOS Docker Desktop, Colima or OrbStack; on Linux Docker Engine. The first-run check (first-run-check.md) gains a Docker line: `docker version` answers and `docker run` of the project's image works (pulled once).
- **As built.** `src/agent_ci/docker.rs` probes `docker version` (program injectable) and answers `ok`, `missing` (not found, no permission, or inside the agent's own image, detected by `/.dockerenv`) or `stopped` (the daemon doesn't answer), each with its reason; `probe_image` runs `docker run --rm <image> true`. The Docker line shows in the setup check. `src/agent_ci/priority.rs` starts the loop's `git` and `docker` children at below-normal priority: `BELOW_NORMAL_PRIORITY_CLASS` on Windows, `nice -n 10` elsewhere.
- **As built, the run.** `src/agent_ci/container.rs` runs `docker run --rm --name retaskable-ci-<repo>-<short sha> --cpu-shares 128 -v <dir>:/workspace -w /workspace -e CI=true <image> bash .agent-ci/checks.sh <pull_request|push>` through the low-priority helper, with the child's environment cleared to `PATH`, the home/profile/system variables and `DOCKER_*` that docker itself needs. Output streams to a log file under the given log folder; the result is the exit code, the log path and the last 120 lines. The ceiling (45 minutes) kills the run and `docker rm -f <name>`s it; `remove_stale(name)` does the same for a cut check. Logs older than 7 days are pruned at the start of each run. On Windows the mount is written `C:/path` (no `\\?\` prefix).
- **Without Docker** (missing, stopped, no permission, or the image can't be pulled): `ciSlots` is treated as 0 and the settings page and `ci.docker` say why ("Docker isn't running: this computer won't run CI"). It never runs `checks.sh` on the host as a fallback: that would put the code under test beside the token and the user's files. Other computers carry on; if none can, the orchestrator's wait ends at `ciMaxMinutes` as it does for any CI that never starts. The loop starts checking again, with no restart, when Docker answers.
- **Windows paths.** Mounts are built from `std::path` values and handed to `docker -v` in the form Docker Desktop accepts; the directory sits under the agent's data folder on a local drive, not a network share. Line endings of `checks.sh` are as committed (the fetch step doesn't convert them).
- **Inside the agent's own Docker image** ([docker.md](docker.md)): nested `docker run` needs the host's socket, which that image must not get (the token rule). So an agent running in Docker runs no CI (`docker: "missing"`), and the computer's other, non-Docker agent does.

### 3.7 The process: link, repos and settings

`retaskable-ci` has three commands. `link` links it to the shared backlog store, once. `run` is the loop. `once <sha>...` polls once, checks only the pull requests or main whose head starts with one of the shas, waits for them and exits.

- **Link, role `ci`.** `link` runs the store's device flow (`src/link/`, the same one the Chat Agent uses) asking for `role: "ci"`, backlog target only; it never links to retaskable.com's Worker. Its refresh token is kept apart from the Coding Agent's: OS credential service `retaskable-ci` (file override `RETASKABLE_CI_CREDENTIALS_FILE`), and its own data subfolder `<data dir>/ci`, so linking it never replaces the agent's link on the same computer. The loop warns when it isn't linked, and the store ops of section 3.4 are its store calls.
- **Repos.** `AGENT_CI_REPO`, comma-separated, each `owner/repo` or `owner/repo@branch` for a base other than `main`. There is no config file and no project list.
- **Its own mirror.** There is no project checkout, so each repo has a bare git directory, `<data dir>/ci/mirrors/<owner>-<repo>.git`, made on first use with `git init --bare` and `origin` set to `https://github.com/<owner>/<repo>.git`. Each poll fetches the base branch into `refs/remotes/origin/<base>` (`mirror.rs`); discovery reads `.agent-ci/image` and `checks.sh` from there with `git show`, `gh` runs with the mirror as its directory, and a check's fetch step clones from it, fetching the head from the remote itself. The fetch uses the computer's own git credentials, as the fetch step always did.
- **Settings.** `AGENT_CI_SLOTS` (default 2, 0 = nothing), `AGENT_CI_CPUS` (default 8, capped at the NCPU `docker info` reports; each check runs with `--cpus`), `AGENT_CI_NAME` (default the host name; the `<computer>` in each status), and `RETASKABLE_DATA_DIR` for the data folder.
- **Which pull requests.** Not drafts, not forks, and a `retaskable/` branch only once it has `ci-ready`; this replaces the per-project `ciWhenReady` switch with the runner's own rule.

## 4. Slices

1. **Slot, discovery and settings.** `ciSlots` in `config/machine.rs` and its environment variable; `src/agent_ci/mod.rs`'s loop with discovery; the Docker check (first-run line, `ci.docker`); the Now tab and tick fields. No container run.
2. **One check.** Fetch, run and report for one claimed pull request: `host.rs`, `container.rs` and `report.rs`, the statuses and the failure comment (sections 3.2, 3.3 and 3.5).
3. **Claims across computers.** `claim.rs`: pending claims, refresh, stale takeover, `ci` events (section 3.4).
4. **Merging and the hold.** `automerge` squash-merge and the person's-merge hold (section 3.3), the push-to-main run.

## 5. Acceptance

- With every lane building, a ready pull request still gets checked: `ciSlots: 1` runs one container beside any number of busy lanes.
- `ciSlots: 0`, or no Docker, runs no check, runs nothing on the host, and says why on the settings page.
- A fake container run (a `docker` stub on PATH) sees no `GH_TOKEN`, `GITHUB_TOKEN`, `GLAB_TOKEN`, credential helper or token in its environment, mounts or the directory's `.git/config`.
- Two loops sharing one stubbed host, given one commit key, run it once; a claim left 15 minutes old is taken over; a finished key is never rerun.
- A red check leaves one comment of at most 120 log lines on the pull request, replaced, not repeated, on a rerun of the same key; a green `automerge` pull request is squash-merged; main moving after the check checks it again.
- A person's pull request waits while an up-to-date agent one is being checked and is released after.
- The Rust agent's own pull request is checked by `.agent-ci/checks.sh` and reports the same statuses a TypeScript-run one does.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass, with the Docker-dependent tests skipped when `docker` is absent.
