## 5. What the agent runs for retaskable.com

The local UI did things retaskable.com couldn't, and each became a command the site sends and the agent runs. The commands and their schemas are the cloud's ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6); the site's buttons, pages and waiting lines are the app's. What stays on the computer is the running:

| # | What | What the agent does |
|---|---|---|
| 1 | **Play on this machine** | `play` / `stop_play` / `restart_play`: start, stop or restart the project's play command for main, an item's build or a lane |
| 2 | **Play on phone** | `phone_play` / `phone_stop`: list the phones adb sees (sent up as derived results) and install the project's own app ([play-on-phone.md](../play-on-phone.md)) |
| 3 | **Project settings and commands** | applies a changed command (`checks`, `playCommand`, gallery commands, deploy) only once the computer's owner has approved it on the site ([project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md) section 3.3); a computer's own settings are set at the computer ([agent-settings.md](../agent-settings.md)), and its budget, sessions at once and model thresholds are the owner's (`agent_settings`) |
| 4 | **Link and unlink a project** | `link_project` `{ repo }` checks the repo with its own `gh`, finds its checkout or clones it, scans the folder as Setup does and saves it paused; `unlink_project` refuses while anything runs in the project, then removes it (and its lanes if asked). Nothing on the code host changes |
| 5 | **Galleries** | `shoot`: runs the gallery's command on the base branch (up to 30 minutes). Section 5.3 |
| 6 | **Release notes** | runs the writer session and, on `release_save`, commits the note on a new branch and opens the PR with the code host's CLI. Whether to publish, and the note's page, are the store's and the site's |
| 7 | **Pictures in what you send** | saves the pictures sent with a message as files for the session to read. Section 5.3 |
| 8 | **Promote** and **Restart** | `promote` runs the project's deploy with its checks and `git`; `restart` restarts the agent at once |
| 9 | **Team branch** | `team_sync` and `main_pr`: fixed `git` and code-host actions, no text that runs |

- **Settings fields.** The agent saves what a settings command allows as Setup does (checks, thresholds out of order refused in Setup's words), then reconfigures the pool and sends a new hello. A project on the shared backlog has its project fields and commands in the store ([project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md) section 3.6); the agent reads them from there and asks its owner's approval before it runs a changed command.
- **Base branch.** A changed `baseBranch` is saved only once Setup's checks pass on the project with it: the agent fetches it and makes the integration branch (or `dev/<login>`) from where Setup would.
- **The remote policy.** `remote` (`enabled`, `allowMerges`, `publishGalleries`, `promoteFromSite`, the view-only sites) stays in `.retaskable/config.json` on the computer, edited by hand. The agent refuses every command when `remote.enabled` is off.
- **The first run.** A machine with no project connects to its own hub with no project and takes one command, `link_project`; everything else is "Finish setup first."
- **No repo list goes through retaskable.com.** The owner and repo picker stays on the machine's own `gh`; the site's form takes `owner/name` typed.
- **Editing a spec draft's files by hand** stays off the website, as [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) and `CLAUDE.md` say ("never ... file edits on an agent"): it's done on the code host (the draft's card links to its PR) or by **Revise** with notes. Approved specs are committed to the user's repo, and spec content never goes to the store.
- **The server never sees or touches the user's code.** The computer sends only derived results: snapshots, run results, screenshots on request.

The site's side of each row (the buttons, the greyed-out waiting lines, the Releases tab, Settings → This project, the gap table) is the app's and the cloud's, not copied here.
