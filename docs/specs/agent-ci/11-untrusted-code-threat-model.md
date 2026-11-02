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
