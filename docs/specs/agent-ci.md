# Agent-run CI — Spec

Status: v0.1, a draft, backlog id `agent-ci`. Builds on [agent.md](agent.md) sections 4.1 and 5 (`awaiting_ci`), [agent-settings.md](agent-settings.md) section 3, [agent-telemetry.md](agent-telemetry.md) section 3, [pieces.md](pieces.md) section 3.2 (`watch`) and [docker.md](docker.md). Copies, for the Rust agent, what Retaskable.com's [ci-when-ready.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-when-ready.md) section 10 says about CI run on the agent computers (since 2026-11-02, off Woodpecker).

Theme: Orchestration

Kind: new

## 1. What it is

In Retaskable.com, `.agent-ci/runner.mjs` polls GitHub and checks each ready pull request, merged with current main, in a Docker container at low priority. It posts the commit statuses the orchestrator waits on, comments the tail of a failed log on the pull request, squash-merges `automerge` pull requests on green, and holds a person's merge while an agent pull request that is up to date with main is still being checked. This spec has the Rust agent do the same, on its own slot, so a computer whose every lane is busy building still runs the CI those lanes wait on.

The Rust agent's own repo already carries the project side of it: `.agent-ci/image` (one line, the Docker image: `rust:1-bookworm`) and `.agent-ci/checks.sh` (run as `bash .agent-ci/checks.sh <pull_request|push>`, the same checks as the repo's workflow).

## 2. Decisions

- **[Decided]** CI has its own slot, `ciSlots`, never a lane and never a session slot. It doesn't count toward `lanes` or `concurrency`.
- **[Decided]** It runs at low CPU priority.
- **[Decided]** The code host token never shares a container with the code under test. Only a separate fetch step, on the host, sees it.
- **[Decided]** Several computers may run CI for one repo; they never run the same commit twice, and claim a commit with a pending status.
- **[Decided]** A failure reaches the fix session as a comment with the last 120 lines of the failed log.
- **[Decided]** Windows, Linux and macOS are all supported, and Docker is required.
- **[Proposal]** Run the loop natively in Rust rather than supervising the project's `runner.mjs`. Reasons: `runner.mjs` is Retaskable.com's own file (this repo has only `image` and `checks.sh`, so there is nothing to supervise here); supervising it needs Node on every computer, which a Windows install of this agent doesn't have; and a child process the agent can't see into is harder to stop on restart (agent.md section 7.4) and to hold to the token rule below. The contract stays the project's: its `.agent-ci/image` and `.agent-ci/checks.sh`, so a project's CI is the same whichever agent runs it. A project that has only `runner.mjs` and no `image` gets no agent CI from this agent (section 3.1).
- **[Proposal]** `ciSlots` defaults to 1, whole number 0 to 8; 0 turns agent CI off on this computer.
- **[Proposal]** A claim is stale 15 minutes after its last refresh (section 3.4).
- **[Proposal]** Where this spec says the statuses' names and wording are "the TypeScript runner's", the build copies them from `.agent-ci/runner.mjs` in Retaskable.com rather than inventing new ones, so the orchestrator's wait is unchanged.

## 3. Behavior

### 3.1 Discovery and the slot

- **Discovery.** For each linked project whose repo is on GitHub (`gh`) or GitLab (`glab`), the loop looks in the repo's `origin/<baseBranch>` for `.agent-ci/image` and `.agent-ci/checks.sh`. Both present: the project has agent CI. One missing: it doesn't, and the loop says why once in `agent.log` and on the Now tab. It reads them from main, not from a pull request, so a pull request can't change what runs its own CI.
- **The slot.** `src/agent_ci/` holds the loop (`mod.rs`), the claims (`claim.rs`), the container run (`container.rs`), the fetch step (`host.rs`) and the code host side, statuses and the failure comment (`report.rs`). It is its own tokio task beside the orchestrator loop, with `ciSlots` permits. A permit is held from the claim to the last status posted. Lanes, sessions and `concurrency` neither take nor wait for one.
- **As built.** `discovery.rs` (the `git show` of the two files and the fetch of `origin/<baseBranch>`) and `prs.rs` (`gh pr list` and the check keys) sit beside `mod.rs`. The loop starts only with the `agent-ci` flag on, and a poll probes Docker again each time, so a Docker started later resumes without a restart; `ciSlots` 0 probes nothing and reports Docker as `off`. A project runs CI only while the orchestrator loop's last read of its backlog worked, so an unreachable shared backlog (or the loop off) runs no CI. The reason a project has none is said once in `agent.log` (again if it changes) and kept for the Now tab's CI header (`ci`: slots, running, Docker and its reason, discovery reasons); the minute `tick` carries `ci: { slots, running, docker, lastResult? }`. Only GitHub (`gh`) is read yet.
- **As built, one check.** After each poll `CiLoop::start` takes a permit (`try_acquire_owned`; none free waits for the next poll) for each key this loop hasn't started, and `check.rs` runs it with the permit held to the last status: pending (`agent-ci <computer>: running`) on the pull request's head, fetch of `refs/pull/<n>/head` into main (`host.rs`), a conflict (or a failed fetch) fails the check with that reason, the failure comment and no container, else the container run (`container.rs`), then success or failure (a non-zero exit, the ceiling or an error) and on failure the comment; the directory is deleted. A started key is not run again by this loop. Each running check shows on the Now tab's CI header (`checks`: project, pull request, short sha, seconds), and `lastResult` (`project`, `pr`, `sha`, `ok`) rides the tick. `started` and `finished` `ci` events carry `item` (`PR #<n>`), `key` (`<head7>-<main7>`), `pr` and `ms`. At start, `recover` removes the container named in each leftover `<dir>.name` file with `docker rm -f` and sweeps the leftover directories.
- **Low priority.** The loop's children (`git`, `docker`) start at below-normal priority, as the agent's own sessions do (agent.md section 4), and the container gets `--cpu-shares 128` so it yields to the computer's own apps and to the agent's sessions.
- **What is checked.** Each poll (every `pollIntervalSec`) lists the project's open pull requests that are ready (`ci-ready` label when `ciWhenReady` is on, as in agent.md section 5; otherwise every non-draft one) and, per pull request, the head sha and current main's sha. The commit to check is the pull request's head merged with main. The key of a check is the pair (head sha, main sha); a new main sha makes it a new check. A pull request that conflicts with main fails the check with that as its reason, without a container.
- **A person's merge.** While an agent pull request that is up to date with main is queued or being checked, a person's pull request waits: the loop leaves its merge-hold status pending (the TypeScript runner's) until the agent's is checked, then releases it.

### 3.2 One check, and the token

1. **Fetch (host, token).** In a fresh directory under the project's data folder (`std::path`, never a hard-coded separator), clone from the repo's existing object store with `git`, fetch the head and main using the code host's own credentials, merge head into main locally and detach. The directory's `.git` is then rewritten to carry no remote URL, no credential helper and no token: the fetch step's own credentials never land in it.
2. **Run (container, no token).** `docker run --rm` with the directory mounted as the working directory, the project's `image`, `--cpu-shares 128`, a wall-clock ceiling of 45 minutes (**[Proposal]**), no host mounts beyond that directory, none of the agent's environment (the allowlist is `CI=true` and what `checks.sh` is told: `pull_request` or `push`), and `bash .agent-ci/checks.sh <pull_request|push>`. The container has no `gh`, `glab`, token, SSH agent or Docker socket. It may reach the network, since checks install their own tools.
3. **Report (host, token).** The exit code and the log's tail go back through the code host's CLI, never from the container (section 3.5).

The directory is deleted when the check ends, on a restart's recovery, and for any left from before at start; a container still running from a cut check is removed by its name (`retaskable-ci-<repo>-<short sha>`).

A push to main runs the same steps with `push`, on main alone, for the baseline the orchestrator reads (agent.md section 6.7, fix main).

### 3.3 Statuses, merging and the hold

The loop posts the commit statuses the orchestrator waits on, with the TypeScript runner's contexts and descriptions: pending when claimed, success or failure when done, each linking to where the log is kept. A green pull request labelled `automerge` is squash-merged by the loop, only once main hasn't moved since its check (else it's checked again). **The hold** (`hold.rs`): after each poll's list, an agent pull request (branch prefix `retaskable/`, as the runner's `agentTurn`) that is ready, 0 behind main (`compare`; an unreadable answer is not up to date) and has no `ci/agent/pr/ci` status yet or a pending one holds every non-draft person's pull request: a pending `ci/agent/turn` status ("agent-ci <computer>: waiting for the agent's turn") on its head, posted once. When no such agent pull request is left (checked, failed, behind main, or gone), a person's pending hold (a draft's too) is posted success ("agent-ci <computer>: the agent's turn is over"). The TypeScript runner holds by waiting before its merge, so it has no status of its own; the context is its pipeline's `turn` step. A host that can't be read holds no one. The orchestrator's own merge (agent.md section 5, `merging`) still goes through its merge lock (pieces.md section 3.4); the loop's `automerge` takes the same lock and waits for it.

### 3.4 Several computers, one repo

A commit key is claimed by posting a pending status naming this computer (its name, section 3.2 of docker.md for a container) on the merged commit's head. Then it reads the statuses back: the earliest pending claim for that key wins, ties broken by computer name, and a loser stops without running anything. A claim is refreshed every 5 minutes by posting again; one not refreshed for 15 minutes is stale and may be taken over, so a computer that's off or cut doesn't strand a commit. A finished check (success or failure) for a key is never run again by any computer. The claim is the only coordination between computers: no store call. A computer that can't reach the shared backlog still runs no CI (fail closed, README.md's rules).

### 3.5 Failures reach the fix session

A failed check comments the last 120 lines of the log on the pull request, once per check key, through the code host's CLI, headed by the check's name and short sha and marked so the loop can find and replace its own comment. The fix session reads the pull request's comments as it already does for a red CI (agent.md section 5, `awaiting_ci`: a failure goes back to `implementing` with the failed checks' names and links), so nothing in the orchestrator changes. The full log is kept on this computer for 7 days, and the status links to it only when the computer is reachable; the comment is what survives.

What the computer reports:

- **Events.** The `ci` event (agent-telemetry.md section 3) gains `claimed`, `started`, `finished` and `stale_taken_over` states, with the item or pull request, the key and `ms`.
- **To the backlog.** The cloud tick carries `ci`: `{ slots, running, docker: "ok" | "missing" | "stopped", lastResult? }`. Nothing else about a check is sent: the log and code stay here (README.md's rule: the server never sees the user's code). The statuses on the code host are what the store's own CI reading uses.
- **Settings page.** The Now tab shows each running check (pull request, short sha, how long) apart from the lanes' sessions, and the computer-only settings gain **CI slots** (`ciSlots`, `RETASKABLE_CI_SLOTS`), kept in `MACHINE_KEYS`.

### 3.6 Platforms and Docker

- **Docker is required.** The loop uses the `docker` CLI on PATH. On Windows that's Docker Desktop in Linux-containers mode (the image is Linux, as the project's `checks.sh` is bash); on macOS Docker Desktop, Colima or OrbStack; on Linux Docker Engine. The first-run check (first-run-check.md) gains a Docker line: `docker version` answers and `docker run` of the project's image works (pulled once).
- **As built.** `src/agent_ci/docker.rs` probes `docker version` (program injectable) and answers `ok`, `missing` (not found, no permission, or inside the agent's own image, detected by `/.dockerenv`) or `stopped` (the daemon doesn't answer), each with its reason; `probe_image` runs `docker run --rm <image> true`. The Docker line shows in the setup check only with the `agent-ci` flag On. `src/agent_ci/priority.rs` starts the loop's `git` and `docker` children at below-normal priority: `BELOW_NORMAL_PRIORITY_CLASS` on Windows, `nice -n 10` elsewhere.
- **As built, the run.** `src/agent_ci/container.rs` runs `docker run --rm --name retaskable-ci-<repo>-<short sha> --cpu-shares 128 -v <dir>:/workspace -w /workspace -e CI=true <image> bash .agent-ci/checks.sh <pull_request|push>` through the low-priority helper, with the child's environment cleared to `PATH`, the home/profile/system variables and `DOCKER_*` that docker itself needs. Output streams to a log file under the given log folder; the result is the exit code, the log path and the last 120 lines. The ceiling (45 minutes) kills the run and `docker rm -f <name>`s it; `remove_stale(name)` does the same for a cut check. Logs older than 7 days are pruned at the start of each run. On Windows the mount is written `C:/path` (no `\\?\` prefix).
- **Without Docker** (missing, stopped, no permission, or the image can't be pulled): `ciSlots` is treated as 0 and the settings page and `ci.docker` say why ("Docker isn't running: this computer won't run CI"). It never runs `checks.sh` on the host as a fallback: that would put the code under test beside the token and the user's files. Other computers carry on; if none can, the orchestrator's wait ends at `ciMaxMinutes` as it does for any CI that never starts. The loop starts checking again, with no restart, when Docker answers.
- **Windows paths.** Mounts are built from `std::path` values and handed to `docker -v` in the form Docker Desktop accepts; the directory sits under the agent's data folder on a local drive, not a network share. Line endings of `checks.sh` are as committed (the fetch step doesn't convert them).
- **Inside the agent's own Docker image** ([docker.md](docker.md)): nested `docker run` needs the host's socket, which that image must not get (the token rule). So an agent running in Docker runs no CI (`docker: "missing"`), and the computer's other, non-Docker agent does.

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
