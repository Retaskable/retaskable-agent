## 12. Open questions

Getting an integration branch into the default branch is no longer a question here: it is a setting of that GitHub workflow pattern, which opens the pull request and rebases it when the default branch moves, and a person can squash-merge that pull request from Retaskable ([github-workflow.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/github-workflow.md) sections 4.2 and 8.1).

- Retaskable helping get a developer's branch into the team's `main` is build order team-branch ([team-branch.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/team-branch.md)): it opens the PR to `main` when asked, merges `main` into the branch when `main` moves (a rebase for a project saved with the **Integration branch** pattern, github-workflow.md section 4.2), and squashes the branch into `main` when a person presses **Add it in**.
- Should clarification questions also be posted on the PR, and answers accepted from GitHub? (v1: the app only, but mirror the questions as a PR comment when a PR exists, for the record.)
- Is the scout split right? Watch in the void-miner dogfood whether Opus overturns many scout findings (they're too noisy) or finds bugs no scout flagged (the concerns are missing something), and adjust the scout prompts.
- Could a shared compile cache (e.g. `sccache` for Rust) shrink each lane's build output further? It's opt-in per repo and not needed for v1.
- On macOS and Linux, does the sandbox need extra allowances for cargo's registry and build dirs, or for `gh`? (Untested: it isn't used on Windows.)