## 6. Build order

| Step | What | Depends on |
|---|---|---|
| **1** | **Local, the same as the others** (the cloud's: [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 9) (built) | |
| **2** | **Requests**, the request kind of command (built, then retired 2026-10-22: commands run at once with the agent's own checks) | |
| **3 to 6** | **The site's commands**: play, settings, galleries, release notes, pictures, Promote and Restart (built, section 5) | |
| **7** | **The tray agent**: the helper, the icon, the menu, `--no-tray` (section 4) (built, section 4.6, with a connector per environment) | |
| **8** | **No local UI**: the agent stops serving the app (`app/dist`, on its port and through the local site at 5173); the app's local mode goes; the README says run the agent, link from the tray, open retaskable.com, where the app is | 3, 4, 5, 6, 7 |

### 6.1 Step 8 in detail: no local UI

Step 8 (no-local-ui) is the last of this spec: once it's built, retaskable.com (in whichever environment the agent is linked to) is the only screen, and the agent is the tray, its console and `npm run link`. The app's side (its local mode, Setup and the link and profile screens, the Source switch, the saved "This computer" choice) is the app's: [expo.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expo.md). This section keeps the agent's side. Everything in it is **[Proposal]**, the drafter's from reading the code on 2026-10-07, and the human's call, except what it quotes from section 2's decisions.

#### 6.1.1 What the local UI was

- **Two servers.** The agent's own (`agent/src/api.ts`, `createApi`, on `127.0.0.1` at `PORT`, `cfg.port` or 4700) answered about 90 `/api/*` routes and served `app/dist` for every other path. The local site (`agent/src/localSite.ts`, port 5173 or `SITE_PORT`, on every interface) served the same `app/dist` and forwarded `/api/*` to the agent, for the phone on the same Wi-Fi and the Expo dev server on 8081.
- **Who called the agent's HTTP API.** Only the app's local mode and tests. The tray, `npm run link`, the console and every cloud connector call the orchestrator in process. The tray helper speaks JSON over stdin and stdout (section 4.6), not HTTP.

#### 6.1.2 What stays at the machine on purpose

The `remote` policy (edited in `.retaskable/config.json`), and editing a draft's files by hand (on the code host or by **Revise**, section 5). The leftover local routes each got a command the site sends instead (`team_sync`, `main_pr`, `release_status`, `phone_stop`, `gallery_draft`, `engines` in the settings command, `link_project`, `unlink_project`, section 5), except the item's whole log from disk, which is lost (section 7).

#### 6.1.3 What goes, and what the agent's port is

- **The agent stops serving the app.** `serveStatic`, `APP_DIST`'s use in `api.ts` and the "The app isn't built" page go. `npm start` stops building the app first, so a first run needs no Expo export. The self-update (`agent/src/selfUpdate.ts`) no longer builds the app after a merge touching `app/` or `shared/`: nothing on the machine serves the build.
- **The agent's HTTP API goes, but for its health check.** `createApi(version)` answers only `GET /api/health` with `{ ok: true, version }` on `127.0.0.1` at `PORT` (the env, `cfg.port` or 4700); every other path is a 404 `{ error: "Not found" }`. It serves no `app/dist`, no `/api/events` and no `/api/state`. (Since 2026-10-22 the computer's own settings page is there too, section 2's decision 8.) `fromLocalPage` (`agent/src/requests.ts`) and the local Allow, Deny, Promote and Go live origin checks, `agentUiOrigin` (`shared/backlogClients.ts`), the backlog store's CORS for the agent's loopback origins, `BacklogLink.token()` and `GET /api/backlog/token` are gone. `notes` and `readRepoFile` moved to `agent/src/repoFiles.ts`. Decided (6.1.5): the port stays for one thing, so a script or a person can ask whether the agent is running. Section 2's decision 4 ("the HTTP API stays bound to `127.0.0.1` only") stays true, and it never takes a command.
- **`allowedHosts` is retired.** It is gone from `MACHINE_KEYS`, the config types and `projectSettings`, and `RETASKABLE_ALLOWED_HOSTS` is no longer read; a config file that still has it loads as before and the console says once at start "`allowedHosts` is no longer used: retaskable.com is the screen now." (`retiredKeysNote`, `agent/src/config.ts`).
- **The local site is only the Sample's server.** `localSite.ts`: `createSite({ dist })` serves a build with its page fallback, GET and HEAD only (else 405), at `SITE_PORT` (5173) from `SITE_DIST` (`app/dist`), with `--sample`; no forwarding of `/api`, no Host allowlist, no CORS. `npm run launch` and the app's scripts use it; the agent workspace's `npm run site` script is gone.

#### 6.1.4 What a person sees

- **The agent's console** at start: "Retaskable agent running (version). Link it from the tray, or `npm run link -- <environment>`." With no project yet: "No project yet: open retaskable.com and add one under Settings → Link project…". Headless, the link state line (section 4.4) is unchanged.
- **The tray**: unchanged (section 4.6). **Open on retaskable.com** is now the only way to the screen; with nothing linked it stays greyed out, "link first".
- **An old tab or bookmark** on `localhost:4700` or `:5173` gets the browser's own "can't connect" page. The README's "Running the agent" says where it went.

#### 6.1.5 Open for the human

Answered 2026-10-08:

- **The item log** after its session: lost. "Only show the log while the work is running, and say older logs are no longer shown."
- **Port 4700**: "Keep only a tiny 'is it running?' check", `GET /api/health`.

#### 6.1.6 Acceptance

- With the agent running and linked, only `GET /api/health` answers on `127.0.0.1:4700`, and nothing on `:5173`.
- `agent/src/api.test.ts` tests the health check and the 404s, and `cloud/test/threatModel.test.ts` checks the agent's port has no Allow to press.
- The console's start lines are 6.1.4's, and an old config's `allowedHosts` is reported once and otherwise ignored.
- Every behaviour that had a `createApi` test is still tested through `Commands` or the functions themselves.
