# Agent settings — Spec

Status: v0.2, decided (2026-10-22), trimmed for the agent. Builds on [tray.md](tray.md) (decision 8, the deliberate exception to "no website of its own"), [agent.md](agent.md) section 8 (the port), [computer-tags.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/computer-tags.md) (tags) and [engines.md](engines.md) (Cursor's models).

Theme: Orchestration

Kind: new

## 1. What it is

A computer running Retaskable has settings of its own, apart from any project's. Until now they were spread over the config file, requests someone at the computer allowed, and three screens on the site. This splits them in two, by who should change them:

- **On the site, by the agent's owner** (the person who linked the computer): what costs money or decides how hard the computer works.
- **At the computer only**: what points it somewhere, or what only makes sense where it runs. Setup asks, an environment variable can set it, and a small page the tray opens changes it.

A project's own settings stay in Project settings on retaskable.com, as before.

## 2. Decisions

- **[Decided]** (2026-10-22) The split below. Nothing in the computer-only half can be changed from the site, so a stolen site session can't point a computer at another cloud, another model server, or open it to the network.
- **[Decided]** (2026-10-22) The human: "could we use a tiny webserver?", then "that way you could navigate to your local ip with the agent running and change the settings remotely from within the network". So the computer-only settings get a page of their own, served by the agent (tray.md decision 8): on `127.0.0.1` by default, on the local network only when the person turns that on at the computer and sets a password.
- **[Decided]** The page changes settings only. It never runs a command, and it never shows or changes a project's settings or the owner's.
- **[Decided]** (2026-10-22) The human: daily budget, sessions at once and model thresholds are agent settings changed on the site, "only changeable by the owner of the agent" (the person who linked it). Everything else about a computer (links, port, local model, how often it checks, tags, Cursor settings, network access) is set only at the computer, never from the site.
- **[Decided]** (2026-10-22) The owner's settings need no fresh sign-in: they run no code. They cost money, so they stay the owner's alone.

| Setting | Where it's changed | Kept in |
|---|---|---|
| Link to retaskable.com | the tray's **Link to**, or `npm run link` | the credential store |
| Link to the shared backlog | by itself once linked, or the tray's **Waiting for you** | the credential store |
| Port | setup, `RETASKABLE_PORT`, the settings page | `config.json` `port` |
| Local model: on or off, the model, its address | setup, `RETASKABLE_LOCAL_MODEL`, `RETASKABLE_LOCAL_URL`, the settings page | `config.json` `local` |
| How often it checks for work | setup, `RETASKABLE_POLL_SEC`, the settings page | `config.json` `pollIntervalSec` |
| Tags | setup, `RETASKABLE_TAGS`, the settings page | `machine.json` `tags` |
| Engines (which program runs each kind of work: Claude Code, Cursor or the local model) | setup, `RETASKABLE_ENGINES`, the settings page | `config.json` `engineChoice` |
| Cursor's models | setup, `RETASKABLE_CURSOR_MODELS`, the settings page | `config.json` `cursor.models` |
| Network access to the settings page, and its password | setup, `RETASKABLE_NETWORK_PASSWORD`, the settings page | `machine.json` `network` (the password as a scrypt hash) |
| Daily budget | Settings → Agents on the site, by the agent's owner only (`agent_settings`) | `config.json` `budget.dailyUsd` |
| Sessions at once (concurrency, auto, per model) | Settings → Agents on the site, by the agent's owner only (`agent_settings`) | `config.json` `concurrency`, `autoConcurrency`, `modelConcurrency` |
| Model thresholds | Settings → Agents on the site, by the agent's owner only (`agent_settings`) | `config.json` `models` |

### 2.1 The owner's settings: `agent_settings`

- **The command** ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1): `agent_settings { dailyUsd?, concurrency?, autoConcurrency?, modelConcurrency?, models? }`, each checked as Setup checks it (a budget from 0 to 100000, sessions whole numbers from 1 to 64, percentages from 0 to 100). Nothing else: a tag, the local model or how often it checks is no part of it, and the agent refuses one as "Not a command this agent takes".
- **The hub** takes it only from the person who linked the computer; the agent trusts that check and re-checks nothing about who sent it. Who may send it, the owner's name in the refusal and MCP's exclusion are the hub's ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1).
- **The agent** (`agent/src/commands.ts`) saves it at once, with no one at the computer to allow it: `saveAgentSettings` (`agent/src/siteSettings.ts`) writes the file through `saveConfigFile` (`withAgentSettings`: what isn't named stays; "Name a change." for none, thresholds out of order and auto's least above its most refused in Setup's words), then `pool.configure` and a new hello on every project, so the page shows the new values.
- **The old ways go.** The agent refuses `computer_tags` and a `settings_request` naming a computer-only setting (`pollIntervalSec`, `local`, `cursorModels`, engines): neither command's schema has them any more.
- **An older agent** doesn't know `agent_settings` and refuses it. An agent that does says so in its hello (`capabilities` has `agent_settings`, `AGENT_SETTINGS_CAPABILITY`).

The Agents page on the site (one card per computer, which settings are editable by the owner or read-only with how to change them at the computer, the Sample's made-up computer) is the app's: [expo.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expo.md). The agent sends what it needs in its hello: the settings and, for the secrets, only each environment variable's name and whether it is set, never a value.

## 3. The computer-only settings

`agent/src/machineSettings.ts` reads, checks and saves them.

### 3.1 Their values

- **Port**: a whole number from 1024 to 65535. Changes once the agent restarts.
- **Lanes**: a whole number from 1 to 64, 2 until set: how many items the computer works on at once, across all its projects (the projects share them: lane k is one lane, and a project can't start work in one another holds). `MACHINE_KEYS` has it, at the top of `config.json`. A config whose projects still have their own `lanes` (or `migrated.lanes`) reads the computer's as the largest of them; those keys are then ignored, noted once on startup (`retiredKeysNote`) and dropped by the next save. The engine of each lane is `engines.lanes` below, and the page shows that many selects.
- **Local model**: on or off; its model's name (up to 100 letters, digits and `. _ : / -`); its address, `http` or `https`, with no user or password in it.
- **How often it checks**: 2 to 3600 seconds.
- **Tags**: [computer-tags.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/computer-tags.md)'s rules. A list typed in is saved as your changes to the found tags (`added`, `removed`), as before. Which work a computer with these tags takes is the store's to decide.
- **Engines**: a default (`claude`, `cursor` or `local`) and, per kind of work (`planner`, `implementer`, `review`, `drafter`, `chat`, `writer`), the default or one of the three. `local` is Claude Code pinned to the local model: it falls back to the role's tier when the local model is off or down, and a resumed session stays on the backend it started on. Environment: `RETASKABLE_ENGINES`, one engine for all (`local`) or `name=engine` pairs (`default=cursor,implementer=local`). Per lane (Built): `lanes`, the engine of each lane, lane 1 first (`claude`, `cursor` or `default`), so 2 lanes can run on Claude Code and 2 on Cursor ([engines.md](engines.md) section 3.1); the page has a select per lane (at least 4, or as many as the most lanes a project has). Environment: `default=claude,lanes=claude+claude+cursor+cursor`; a lanes list with any entry that is not `claude`, `cursor` or `default` is left out whole, and the page refuses one. Until the computer saves its own choice, a project's older `engines.default`/`roles` still count. The website only shows it and refuses to set it (`ENGINES_AT_THE_COMPUTER`); a project's `engines.allow` (Cursor's allowlist) stays a project setting.
- **Cursor's models**: one per tier (planning and review, building, scouts and small jobs); empty is `auto`.
- **Network access**: on or off. On needs a password of at least 8 characters, kept as `scrypt$N$r$p$salt$hash` and checked in constant time. Turning it off keeps the hash, so turning it on again needs no new one. Changes once the agent restarts.

### 3.2 Environment variables

For a computer with no screen, each can be set by a variable, read through `envVar` (so the product's old variable names count too):

| Variable | Sets |
|---|---|
| `RETASKABLE_PORT` | the port |
| `RETASKABLE_LOCAL_MODEL` | the local model on, with this model; `off`, `false`, `0` or `no` turns it off |
| `RETASKABLE_LOCAL_URL` | the local model's address |
| `RETASKABLE_LANES` | the lanes, a whole number from 1 to 64 |
| `RETASKABLE_POLL_SEC` | how often it checks |
| `RETASKABLE_TAGS` | the tags, comma between, instead of the found ones and your changes |
| `RETASKABLE_CURSOR_MODELS` | one model for every tier (`gpt-5`), or each by name (`top=gpt-5,implementer=sonnet,scout=gpt-5-mini`) |
| `RETASKABLE_NETWORK_PASSWORD` | network access on, with this password |

- **A variable wins over the file.** The agent runs on the file's settings with the variables' over them (`withEnvSettings`, in `Pool.configure`); the file keeps its own, so a save never writes a variable's value into it. A value that doesn't read (a port that isn't a number) is left out.
- **It says so.** The settings page shows such a setting greyed out, "Set by RETASKABLE_POLL_SEC: change it there.", and refuses a change to it; setup says it instead of asking. Each is in `AGENT_ENV_VARS`, so the hello lists it as set, by name only, never its value.

## 4. The settings page

### 4.1 What it is

- `GET /settings` on the agent's port: `agent/settings.html`, plain HTML with a little inline JS and no app code, nothing loaded from elsewhere (inlined in a release build by `scripts/bundle.ts`). Its CSP allows only its own nonce'd script and style and calls to itself; it can't be framed or cached.
- `GET /api/settings` gives the settings, which a variable sets, and whether a restart is waiting (the saved port or network access differs from what the agent runs on). `POST /api/settings` saves a change through `saveSettings` (the config file through `saveConfigFile`, tags and network access in machine.json), then `pool.configure`, then a new hello. The port and network access say "Saved. The port and network access change once Retaskable restarts." with **Restart now**, which is `POST /api/settings/restart`, the same `requestRestart` as the site's Restart.
- `/api/health` is unchanged. Every other path is still a 404. The port never takes a command.

### 4.2 Signing in on this computer

- The tray's **Settings…** (and the terminal menu's) opens `http://127.0.0.1:<port>/settings#<token>`: a random token, made for that click, good once within 5 minutes. A token in the fragment never reaches a server log or a `Referer`.
- The page takes it out of the address and posts it to `/api/settings/session`, which answers with a session cookie (`HttpOnly`, `SameSite=Strict`, 8 hours) and a CSRF token. A tray token is taken only from the computer itself (a loopback address).

### 4.3 What every request must have

- **Its Host is this computer's own**: `127.0.0.1`, `localhost` or `[::1]` at the port it answered on, and while network access is on, the computer's own network addresses. Any other (a DNS name pointed at 127.0.0.1) is refused: no DNS rebinding.
- **A POST's Origin is its own** (`http://<the Host>`), and its body is JSON of at most 16 KB, so another page can't post a form to it.
- **A change has the session's CSRF token** in `X-CSRF-Token`, compared in constant time.
- **From another computer**, only while network access is on, and signed in only with the password; a tray token doesn't work there.
- **Wrong tries wait.** Five wrong tokens or passwords from one address within a minute and it waits a minute, then two, then four, up to an hour; even the right password waits.

### 4.4 Network access

Off by default. When it was on with a password as the agent started (or `RETASKABLE_NETWORK_PASSWORD` is set), the port listens on every address (`0.0.0.0`) instead of `127.0.0.1`, and the console says the page is open on the network. Turned off while running, other computers are refused at once; the binding changes at the restart. The page is plain `http` on the local network: a password typed there crosses it unencrypted, as on any router's own page, so it's a password for this page only. The port binds `127.0.0.1` unless the person at the computer turns this on, and never takes a command either way.

A release build opens the page once by itself, on its first run with no config file yet (`settingsOpened` in machine.json).

## 5. Setup

`npm run link` (`agent/src/cli.ts`), once linked to retaskable.com and with someone at a terminal, asks each in turn with what it is now in brackets, which Enter keeps (`agent/src/setupQuestions.ts`): the port, the local model (on or off, then its model and address), how often to check for work, the tags ("none" for none), Cursor's model per tier ("auto" for auto), and network access with its password (not echoed, asked again when too short; on without one stays off). A setting a variable sets is said, not asked. Without a terminal nothing is asked. It saves as the page does and says "Saved. If Retaskable is running, restart it to use them." A running agent's own saves of machine.json keep the tags and network access setup wrote meanwhile.

## 6. Tested

The agent part: `agent/src/commands.test.ts` (`agent_settings` saved at once, nothing waits at the machine, and its wire takes only the owner's fields; the old `computer_tags` and computer-only fields refused), `agent/src/siteSettings.test.ts` (`withAgentSettings`, and neither schema carrying what's set at the computer).

The computer part:

`agent/src/settingsServer.test.ts` (a missing, wrong, used or old token; the cookie's flags; a wrong Host, Origin or CSRF token, or no JSON; network access off, a tray token from another computer, the password, and the wait after five wrong tries), `agent/src/machineSettings.test.ts` (each variable over the file and reported, what the agent runs on, the checks, saving, the hash, a running agent's save keeping setup's), `agent/src/setupQuestions.test.ts` (answers saved, Enter keeping everything, variables said not asked, network access without a password), `agent/src/tray.test.ts` and `terminalMenu.test.ts` (Settings… and its link with a new token each time).
