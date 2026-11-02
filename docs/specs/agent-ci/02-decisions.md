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
