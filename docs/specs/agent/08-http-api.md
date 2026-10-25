## 8. HTTP API

The agent's HTTP port has the health check and, since 2026-10-22, the computer's own settings page ([agent-settings.md](../agent-settings.md) section 4, tray.md decision 8). `createApi(version, settings)` (`agent/src/api.ts`, the page's routes in `agent/src/settingsServer.ts`) listens on `127.0.0.1`, or on the local network only when the person at the computer turns network access on and sets a password, at `PORT` (the env, else the config's `port`, else 4700, or 4701 in a Staging release build, [installers.md](../installers.md) section 8.1). It never listens on a public port: the cloud link is outbound only.

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/health` | `{ ok: true, version }`, `version` the agent's package version, so a script or a person can ask whether the agent is running |
| GET | `/settings` | the settings page (`agent/settings.html`), plain HTML and a little JS |
| POST | `/api/settings/session` | signs in: `{ token }` from the tray's link, or `{ password }` with network access on |
| GET / POST | `/api/settings` | the computer's own settings, read and changed (agent-settings.md section 3) |
| POST | `/api/settings/restart` | restarts the agent, for a new port or network access |

Every other path answers 404 `{ error: "Not found" }`, and none of them runs a command. The port serves no app build, no snapshot (`/api/state`) and no event stream (`/api/events`): retaskable.com is the only screen.

Everything else the agent does for a person comes over the outbound cloud link, as one of the commands the website can send ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6), each of which the agent checks itself. The site reads the snapshot and the live session events over the same link. The agent has no route for any of them.

- **Commands that run something on the machine** (Play, Shoot, Promote, Go live, Restart, linking and unlinking a project) are on the same list, and the agent checks each one before it runs ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1; the tray's own allow step, [tray.md](../tray.md) section 5.1).
- **A shared project's own commands** (checks, Play, CI, galleries, the staging smoke, automatic rollout) are no request since 2026-10-22: a new version of them waits in the snapshot (`commandsWaiting`) until the computer's owner approves it on the site, freshly signed in (`approve_commands`, cloud.md section 6.1, [project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md) section 3.3), and until then the agent runs the ones it ran (`agent/src/allowedCommands.ts`).
- The tray helper speaks JSON over stdin and stdout, and the tray, the console and `npm run link` call the orchestrator in process. Without a desktop (`--no-tray`, no `DISPLAY` on Linux) and with a terminal on stdin and stdout, or with `--tui` / `RETASKABLE_TUI=1`, the tray's menu is drawn in the terminal instead, full screen, with the console's last lines under it ([tray.md](../tray.md) section 4.7).
- A restart exits with code 75 (section 4.2: now, finishing a merge first), and `agent/scripts/supervise.mjs` (what `npm run dev` and `npm start` run) starts it again with the current code; a release build, which has no supervisor, starts its next copy itself ([installers.md](../installers.md) section 3.8).
- The commands that send text to Claude can carry `pictures`, which the agent saves in `.retaskable/attachments/` (`agent/src/attachments.ts`) and whose paths it adds after the text, under "Attached images (look at each with your Read tool before acting on this)".
