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
- **[Decided]** (2026-11-09, [pools-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pools-on-server.md)) The owner's three settings moved from the computer to the server: they are kept in the project's store, in the pool record of each computer that has reported to it, and written by `pool_settings`, not by the `agent_settings` command, which is gone. The computer does not read them back; the store applies them when it sends work (the number of sessions at once, the daily budget, the tier thresholds). Settings start from the defaults (1 session, no daily cap, auto on with 1 to 8 at 75%, Opus above 50%, wait below 5%); nothing is read from an old `config.json`. There is no per-model sessions setting and no Sonnet threshold any more. The keys stay in `config.json` and the agent's own scheduling still reads them until the agent's own pool is deleted (not built), so the numbers on the computer no longer show anywhere.

| Setting | Where it's changed | Kept in |
|---|---|---|
| Link to retaskable.com | the tray's **Link to**, or `npm run link` | the credential store |
| Link to the shared backlog | by itself once linked, or the tray's **Waiting for you** | the credential store |
| Port | setup, `RETASKABLE_PORT`, the settings page | `config.json` `port` |
| Local model: on or off, the model, its address | setup, `RETASKABLE_LOCAL_MODEL`, `RETASKABLE_LOCAL_URL`, the settings page | `config.json` `local` |
| How often it checks for work | setup, `RETASKABLE_POLL_SEC`, the settings page | `config.json` `pollIntervalSec` |
| Tags | setup, `RETASKABLE_TAGS`, the settings page | `machine.json` `tags` |
| Engines (which program runs each kind of work: Claude Code, Cursor, pi, Codex or the local model) | setup, `RETASKABLE_ENGINES`, the settings page | `config.json` `engineChoice` |
| Cursor's models | setup, `RETASKABLE_CURSOR_MODELS`, the settings page | `config.json` `cursor.models` |
| Network access to the settings page, and its password | setup, `RETASKABLE_NETWORK_PASSWORD`, the settings page | `machine.json` `network` (the password as a scrypt hash) |
| Daily budget | Settings → Agents on the site, by the agent's owner only (`pool_settings`) | the project's store, in that computer's pool record (`dailyUsd`) |
| Sessions at once (`limit`) | Settings → Agents on the site, by the agent's owner only (`pool_settings`) | the project's store (`limit`) |
| Model thresholds (`opusAbovePct`, `pauseBelowPct`) | Settings → Agents on the site, by the agent's owner only (`pool_settings`) | the project's store (`models`) |

### 2.1 The owner's settings: `pool_settings` (changed 2026-11-09)

- **The op** (`shared/backlogPoolOps.ts`, [pools-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/pools-on-server.md) section 5.4): `pool_settings { agent, limit?, dailyUsd?, autoConcurrency?, models? }`, checked in the ranges of `shared/pools.ts`, saved in the project's store at once. `pool_get` reads them with `allowed` and the reason to wait. The store takes `pool_settings` only from the computer's owner, who is the login of its first `health`; so it is refused until the computer has reported once to that project, and the Agents tab shows the defaults read-only until then.
- **The site** sends it through the shown project's store (a computer that serves several projects has one pool per store, so the tab shows the shown project's). Only the keys that changed are sent. The old `agent_settings` command, its schema, the hello capability and the agent's write (`saveAgentSettings`) are gone, and the hub refuses an old page's `agent_settings` with "...saved in the project, not on the computer", the owner too.
- **The old ways stay gone.** The hub still refuses `computer_tags` and a `settings_request` naming one of these settings or one set at the computer (`pollIntervalSec`, `local`, `cursorModels`, `lanes`), in plain words (`retiredRefusal`).
- **A one-time note**, a dismissible banner kept on the device (`retaskable.poolSettingsNote`), says the settings moved and start from the defaults.

The Agents page on the site (one card per computer, which settings are editable by the owner or read-only with how to change them at the computer, the Sample's made-up computer) is the app's: [expo.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/expo.md). The agent sends what it needs in its hello: the settings and, for the secrets, only each environment variable's name and whether it is set, never a value.

## 3. The computer-only settings

`agent/src/machineSettings.ts` reads, checks and saves them.

### 3.1 Their values

- **Port**: a whole number from 1024 to 65535. Changes once the agent restarts.
- **Lanes**: a whole number from 1 to 64, 2 until set: how many items the computer works on at once, across all its projects (the projects share them: lane k is one lane, and a project can't start work in one another holds). `MACHINE_KEYS` has it, at the top of `config.json`. A config whose projects still have their own `lanes` (or `migrated.lanes`) reads the computer's as the largest of them; those keys are then ignored, noted once on startup (`retiredKeysNote`) and dropped by the next save. The engine of each lane is `engines.lanes` below, and the page shows that many selects.
- **CI slots** (**[Proposal]**, [agent-ci.md](agent-ci.md)): a whole number from 0 to 8, 1 until set: how many pull requests' CI the computer runs at once in Docker, apart from the lanes. `MACHINE_KEYS` has it.
- **Local model**: on or off; its model's name (up to 100 letters, digits and `. _ : / -`); its address, `http` or `https`, with no user or password in it.
- **How often it checks**: 2 to 3600 seconds.
- **Tags**: [computer-tags.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/computer-tags.md)'s rules. A list typed in is saved as your changes to the found tags (`added`, `removed`), as before. Which work a computer with these tags takes is the store's to decide.
- **Engines**: a default (`claude`, `cursor`, `pi`, `codex` or `local`) and, per kind of work (`planner`, `implementer`, `review`, `drafter`, `chat`, `writer`), the default or one of the five. `pi` is the pi coding tool ([engines.md](engines.md) section 13); `codex` is Codex ([engines.md](engines.md) section 14). `local` is Claude Code pinned to the local model: it falls back to the role's tier when the local model is off or down, and a resumed session stays on the backend it started on. Environment: `RETASKABLE_ENGINES`, one engine for all (`local`) or `name=engine` pairs (`default=cursor,implementer=local`). Per lane (Built): `lanes`, the engine of each lane, lane 1 first (`claude`, `codex`, `cursor` or `default`), so 2 lanes can run on Claude Code and 2 on Cursor ([engines.md](engines.md) section 3.1); the page has a select per lane, as many as the computer's lanes (or, if more were already saved, that many instead, so lowering lanes never silently drops a saved override). Environment: `default=claude,lanes=claude+claude+cursor+cursor`; a lanes list with any entry that is not `claude`, `codex`, `cursor` or `default` is left out whole, and the page refuses one. Until the computer saves its own choice, a project's older `engines.default`/`roles` still count. The website only shows it and refuses to set it (`ENGINES_AT_THE_COMPUTER`); a project's `engines.allow` (Cursor's allowlist) stays a project setting.
- **Cursor's models**: one per tier (planning and review, building, scouts and small jobs); empty is `auto`.
- **Cursor may also run** (per project, shown once any role or lane is on Cursor): a text box for the typed `engines.allow` entries (commas or new lines), a refused entry's reason beside it, the `allowlist_in_force` line (red when sessions won't start) and, when there are any, "Already allowed, from your checks: Shell(npm), Shell(cargo)". It saves through `POST /api/settings` (`cursorAllow`).
- **Network access**: on or off. On needs a password of at least 8 characters, kept as `scrypt$N$r$p$salt$hash` and checked in constant time. Turning it off keeps the hash, so turning it on again needs no new one. Changes once the agent restarts.

### 3.2 Environment variables

For a computer with no screen, each can be set by a variable, read through `envVar` (so the product's old variable names count too):

| Variable | Sets |
|---|---|
| `RETASKABLE_PORT` | the port |
| `RETASKABLE_LOCAL_MODEL` | the local model on, with this model; `off`, `false`, `0` or `no` turns it off |
| `RETASKABLE_LOCAL_URL` | the local model's address |
| `RETASKABLE_LANES` | the lanes, a whole number from 1 to 64 |
| `RETASKABLE_CI_SLOTS` | the CI slots, a whole number from 0 to 8 |
| `RETASKABLE_POLL_SEC` | how often it checks |
| `RETASKABLE_TAGS` | the tags, comma between, instead of the found ones and your changes |
| `RETASKABLE_CURSOR_MODELS` | one model for every tier (`gpt-5`), or each by name (`top=gpt-5,implementer=sonnet,scout=gpt-5-mini`) |
| `RETASKABLE_NETWORK_PASSWORD` | network access on, with this password |
| `RETASKABLE_SECRETS_KEY_FILE` | a file holding the key for saved secrets, as base64 (section 3.3) |
| `RETASKABLE_SECRETS_KEY` | the key for saved secrets, as base64 (section 3.3) |
| `RETASKABLE_SETTINGS_PUBLISHED_LOCAL` | `1`: the port is published on the host's `127.0.0.1` only (Docker), so a request whose Host is `127.0.0.1`, `localhost` or `[::1]` counts as this computer for saving secrets (section 4.3) though it reaches the agent from Docker's gateway. Not a setting the page shows. |

- **A variable wins over the file.** The agent runs on the file's settings with the variables' over them (`withEnvSettings`, in `Pool.configure`); the file keeps its own, so a save never writes a variable's value into it. A value that doesn't read (a port that isn't a number) is left out.
- **It says so.** The settings page shows such a setting greyed out, "Set by RETASKABLE_POLL_SEC: change it there.", and refuses a change to it; setup says it instead of asking. Each is in `AGENT_ENV_VARS`, so the hello lists it as set, by name only, never its value. In this Rust agent `AGENT_ENV_VARS` (`hello_settings.rs`) also has `RETASKABLE_LANES`, and the hello's `settings` object holds the owner's settings and the computer-only values the page shows (port, lanes, pollIntervalSec, local, engines, cursorModels, tags, networkAccess) with `envVars`; a save on the settings page sends a fresh hello on every project's connection. `settings` is in the hello. `capabilities` is `["agent_settings"]` (`AGENT_SETTINGS_CAPABILITY`) and `agent_settings` is taken. `agent_settings` is saved at once through `site_settings::save_agent_settings` and fires the same hello signal; there is no session pool yet, so nothing is configured after the save.

### 3.3 Secrets (encrypted in the config; a deliberate divergence from the TypeScript agent)

The TypeScript agent keeps each saved secret as its own OS credential entry (service `retaskable-secret`), or in a 0600 `credentials.json` where there's no OS store. **This Rust agent does not: the user chose to keep them as ciphertext in the config.** Everything the page and the environment do is the same; only where a saved value rests differs.

- **Which**: `GH_TOKEN` (gh, and git over https), `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, `GITLAB_TOKEN`, `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` (`SECRETS`, `src/secrets/mod.rs`).
- **The environment wins.** At start, before anything is spawned (`Secrets::apply`, called in `main` before the first project starts), a variable with a non-empty value is the environment's and is locked on the page ("Set by GH_TOKEN: change it there."); an empty one (compose's unset `${X:-}`) is none. For each of the rest, a saved one is decrypted and put in the agent's own environment, which every session, check and script it starts inherits. `RETASKABLE_SAVED_SECRETS` names the ones the agent put there (names only), so a restarted agent, which inherits that environment, still tells them from the environment's; one the environment has since changed is the environment's.
- **Saved** in `config.json`, in a `secrets` object, `{ "<NAME>": "<ciphertext>" }`: the same file as the rest of the computer's settings, so one place to back up or move, and `save_machine_settings` keeps `secrets` (and any key it doesn't know) through every other save. A sibling `secrets.json` was the other choice; it would add a second file to keep in step with the first and nothing the config's patching doesn't already give.
- **The cipher**: XChaCha20-Poly1305 (the RustCrypto `chacha20poly1305` crate, `src/secrets/cipher.rs`) under a 256-bit data key, a fresh random 24-byte nonce for every seal (so saving the same value twice stores different text), and the secret's NAME as associated data (so a ciphertext copied under another name doesn't open). Stored as `v1:<base64 of nonce, then ciphertext and tag>`; the version prefix lets another cipher live beside it.
- **The key**: one random data key per data folder, never in the config (`src/secrets/key.rs`):
  1. `RETASKABLE_SECRETS_KEY_FILE`: a file holding the key as base64 (32 bytes). On Unix it must be readable by this user only (0600); one that isn't is refused. If the file isn't there yet, the first save creates it with a fresh key (0600).
  2. `RETASKABLE_SECRETS_KEY`: the key as base64. Read once at start and then removed from the agent's own environment, so no session inherits it. (A copy of the agent that restarts itself without its supervisor would not have it; a supervisor such as Docker or systemd brings its own back. The file does not depend on this.)
  3. The OS credential store (Windows Credential Manager, macOS Keychain, the Secret Service), service `retaskable-secrets-key`, account a hash of the data folder's absolute path. Made on the first save, not at start.
  4. None of these (no OS store, no variable): secrets can't be saved, and the page says so, naming both variables.
- **Plainly, on a computer with no OS store (Docker), the key sits beside the config** (a file in the volume, or the container's environment). Anyone who can read the data folder and the key can read the secrets; the protection there is file permissions, not the cipher. What the encryption buys everywhere is that the config on its own (a backup, a copied folder, a screen share, an accidental commit) holds no usable secret. It is not a boundary against a process running as the same user: the sessions get the same tokens in their environment (but not `CLOUDFLARE_API_TOKEN`, which sessions never get) and could read the key where the user can.
- **Fail closed.** A missing or wrong key, a changed ciphertext, one under another name, a format this build doesn't know, or an OS store that won't answer: that secret reads as **Unreadable: save it again**. It is not put in the environment, there is no plaintext fallback, and nothing crashes. Saving it again makes it readable under the key now in use.
- **Moving from the TypeScript agent.** At start, for each name the config has no entry for but the TypeScript agent's store does (service `retaskable-secret`, account the name, in the OS store or in `credentials.json`, or the file `RETASKABLE_CREDENTIALS_FILE` names), the value is encrypted into the config and then the old entry is deleted, with one line in the log naming the secrets (never a value). The link tokens beside them in `credentials.json` stay. An entry the config already has wins and the old one is left alone; without a key to seal with, the old one stays where it is and the log says why.
- **The page shows** each as **From the environment** (locked), **Saved on this computer** with **Clear**, **Not set**, or **Unreadable: save it again**, and a set one's fingerprint: the first 8 hex characters of its SHA-256 (`sha256 1a2b3c4d`). Never the value: the page never has it, and no response, log line or error carries one. A field takes a new one (written only, cleared on every render, empty keeps the saved one).
- **A save** (`secrets` in `POST /api/settings`, `{ <name>: value | null }`, `null` clears) is checked whole, with the rest of the body, before anything is written: a name on the list, not the environment's, 1 to 4096 characters with no spaces, line breaks or control characters, and a key to seal with. It then writes the config and updates the agent's own environment, so it reaches new sessions; **sessions already running keep the environment they started with**. A new `GH_TOKEN` runs `gh auth setup-git`, and so does a saved one at start; a fixed step of the agent's own, not a command from the page.
- **`GET /api/settings`** adds `secrets: [{ name, label, use, source: "env" | "saved" | "unset" | "unreadable", fingerprint? }]`, `canSaveSecrets` (this request is from this computer, section 4.3) and `secretsNote` (null, or why no save can work here, such as no key).
- **Only from this computer** (section 4.3).
- **The other two programs' secrets** (**[Decided 2026-11-16]**, `Owner`, `src/secrets/mod.rs`). The CI runner saves `GH_TOKEN` and the Chat Agent `CLAUDE_CODE_OAUTH_TOKEN` and `ANTHROPIC_API_KEY`, each as `{ "secrets": { "<NAME>": "v1:..." } }` in its own `settings.json` (`ci/`, `chat/`), never in `config.json`. They are sealed under the same data key of the data folder (one OS-store entry, or the same key file or variable), with the same cipher, the same rules (the environment wins and is locked, write-only, 1 to 4096 characters, fail closed to Unreadable) and the same "only from this computer" check. The program applies its own at start (`Secrets::for_program`, `apply`) and again whenever its file's `secrets` changed (`refresh`, each poll for the CI runner, each project-list refresh for the Chat Agent), so a save made on the page, which is another process, reaches the checks and sessions started after it; the names its environment sets go into the `envSet` of its `status.json`. The page edits them through `Secrets::for_page`, which reads and writes the program's file and **never touches the page process's own environment** (a token saved for the CI runner must not become the code agent's), and takes the locked names from the program's status file. All the programs on one computer must find the same key (the default, the OS store, does; with `RETASKABLE_SECRETS_KEY_FILE` or `RETASKABLE_SECRETS_KEY`, give each the same one).
- **The site** gets names and where each comes from (`computer.secrets` in the hello, as the TypeScript agent sends it: `{ key, state: "set", kind: "env" | "key" }` for each one that is set, beside `lanes`) and nothing else: neither a value, a fingerprint nor ciphertext.

## 4. The settings page

### 4.1 What it is

- `GET /settings` on the agent's port: `agent/settings.html`, plain HTML with a little inline JS and no app code, nothing loaded from elsewhere (inlined in a release build by `scripts/bundle.ts`). Its CSP allows only its own nonce'd script and style and calls to itself; it can't be framed or cached.
- `GET /api/settings` gives the settings, which a variable sets, and whether a restart is waiting (the saved port or network access differs from what the agent runs on). `POST /api/settings` saves a change through `saveSettings` (the config file through `saveConfigFile`, tags and network access in machine.json), then `pool.configure`, then a new hello. The port and network access say "Saved. The port and network access change once Retaskable restarts." with **Restart now**, which is `POST /api/settings/restart`, the same `requestRestart` as the site's Restart.
- `/api/health` is unchanged. Every other path is still a 404. The port never takes a command.

### 4.2 Signing in on this computer

- `SettingsServer::settings_link()` makes the address the tray's **Settings…** (and the terminal menu's) opens, and the first run below opens, `http://127.0.0.1:<port>/settings#<token>`: a random token, made for that click, good once within 5 minutes. A token in the fragment never reaches a server log or a `Referer`.
- The page takes it out of the address and posts it to `/api/settings/session`, which answers with a session cookie (`HttpOnly`, `SameSite=Strict`, 8 hours) and a CSRF token. A tray token is taken only from the computer itself (a loopback address); one from another address is refused and not used up.

### 4.3 What every request must have

- **Its Host is this computer's own**: `127.0.0.1`, `localhost` or `[::1]` at the port it answered on, and while network access is on, the computer's own network addresses. Any other (a DNS name pointed at 127.0.0.1) is refused: no DNS rebinding.
- **A POST's Origin is its own** (`http://<the Host>`), and its body is JSON of at most 16 KB, so another page can't post a form to it.
- **A change has the session's CSRF token** in `X-CSRF-Token`, compared in constant time.
- **Secrets are saved only from this computer** (section 3.3): a request from a loopback address, or, with `RETASKABLE_SETTINGS_PUBLISHED_LOCAL=1`, one whose Host is `127.0.0.1`, `localhost` or `[::1]`. Any other answers 403 "Secrets can be saved only on this computer." and nothing of it is saved (not even a clear); `GET /api/settings` says which with `canSaveSecrets`.
- **From another computer**, only while network access is on, and signed in only with the password; a tray token doesn't work there.
- **Wrong tries wait.** Five wrong tokens or passwords from one address within a minute and it waits a minute, then two, then four, up to an hour; even the right password waits.

### 4.4 Network access

Off by default. When it was on with a password as the agent started (or `RETASKABLE_NETWORK_PASSWORD` is set), the port listens on every address (`0.0.0.0`) instead of `127.0.0.1`, and the console says the page is open on the network. Turned off while running, other computers are refused at once; the binding changes at the restart. The page is plain `http` on the local network: a password typed there crosses it unencrypted, as on any router's own page, so it's a password for this page only. The port binds `127.0.0.1` unless the person at the computer turns this on, and never takes a command either way.

A release build opens the page once by itself, on its first run with no config file yet (`settingsOpened` in machine.json, set when it opens, even if no browser could be started). A checkout build never opens it.

### 4.5 The Now tab and `GET /api/settings/status`

The page has two tabs, **Now** (the first) and **Settings**, kept in the address (`#now`, `#settings`; any other fragment is a sign-in token, 4.2). Only this computer's own view: nothing here is sent to the site.

- `GET /api/settings/status` is read-only and answers exactly as the page does: the session cookie, the Host and loopback checks of 4.3, and 401 when signed out. It gives `ComputerStatus` (`src/settings_server/status.rs`, built from the config, the latest setup checks, the agent's own view and `machine.json`): the header (name, state, version, uptime in seconds, projects, lanes busy of total), alerts, lanes, sessions without a lane, and engines. Names and numbers only.
- **Needs attention**: Claude Code signed out, and a paused project with its reason. Nothing is listed when nothing is wrong.
- **Lanes** are the computer's, across every project, each with its engine (its own, or the default followed). In this Rust agent each lane shows the session on it from the live registry (`orchestrator::live::sessions`) and the sessions without a lane list holds every other session (see below).
  - **Live sessions.** Each lane shows the session on it, and a session with no lane (an ask, a spec draft, a scout) is listed apart. A row has the project, the item or draft, the role, the engine and model, how long it has run and how long it has been quiet. It has no transcript text or prompt. The page shows a lane's session as `implementer · codex gpt-x · run 12m, quiet 30s`, and the header's "lanes busy" counts lanes with a session on them. `GET /api/settings/status` gives the same rows. The cloud tick's `sessions` count is the number of rows.
- **Engines**: found and its version (from the setup checks, re-run every ten minutes), signed in (Claude Code only), what it is used for (kinds of work and lanes), its limit ("Not reported": nothing here reads one), and today's tokens for Cursor and Codex when something records them (`machine.json`, by local day, seven days kept).
- The page polls the status every 5 seconds while the Now tab is showing and the page is visible; a failed poll says it isn't updating and keeps the last picture. Times show in the viewer's clock.
- The **Settings** tab holds the same fields as before, regrouped, with a save per group. Every field keeps its id and the save of 4.1. The lanes number is shown, not saved, here.

### 4.6 The three programs on one page (programs.md)

**[Decided 2026-11-16]** The page is served by the code agent and covers all three programs, which are separate processes that may run on different computers. Nothing here talks to a program: the page reads each program's `status.json` (programs-status.md) and edits each program's own settings file (`src/program_settings/`; agent-ci.md section 3.7). The HTTP port still serves only `GET /api/health` and the settings page; `settings_server/routes_tests.rs` fails when a route is added.

- **`GET /api/settings/programs`** (a session, the checks of 4.3) answers `{ programs: [{ id, label, state, lastSeen, version, lastError, details, linkCommand, settings, secrets, secretsNote, canSaveSecrets, applies }] }` for `code-agent`, `chat-agent` and `ci-runner`. `state` is `running`, `stopped` (with `lastSeen`, ms), `unlinked` or `notInstalled` (`program_status::state_in` against the server's clock). `linkCommand` is set only when unlinked: `retaskable link backlog`, `retaskable-chat-agent link`, `retaskable-ci link`. The page shows it as text to run in a terminal on that computer; there is no link button. `details` is the program's own status fields (names and numbers). For the Chat Agent and the CI runner when installed (a status file or a folder), `settings` is `{ file, effective, defaults, locked, env, error }`: `file` is what the settings file holds (what a form edits), `effective` what the program uses (the environment over the file over the default), `locked` the fields its environment sets, `env` each field's variable, and `error` why the file can't be read. `secrets` has the states of section 3.3 and never a value. The code agent's own settings stay in `GET /api/settings`; here it has only its status.
- **Locks come from the program's status file.** The page process's environment is not the program's, so a field is locked when the variable that sets it is in the `envSet` of that program's last `status.json` (names only). A program that never wrote a status has none known, and a saved field its environment turns out to set is ignored by the program until the page next reads its status.
- **`POST /api/settings/programs/{program}`** (`chat-agent` or `ci-runner`; the code agent saves through `POST /api/settings`) takes `{ settings?: { <field>: value | null }, secrets?: { <NAME>: value | null } }`, nothing else. It has the same session, CSRF, Origin, Host and 16 KB checks as every other write. Everything is checked before anything is written; a field the program's environment sets, a name the program doesn't have, a value out of range or a program not installed is a 400. It writes that program's own `settings.json` atomically, and no other file. Secrets go through the checks of 3.3 and only from this computer, sealed under the data folder's one key, and never change this process's environment (`Secrets::for_page`).
- **When a save applies.** The CI runner reads its file before every poll: `repos`, `slots`, `cpus`, `name` and its saved `GH_TOKEN` apply within about 10 seconds (a lower slot count as running checks end). The Chat Agent reads `onlyRepoId` and its secrets at each refresh of its projects, within 5 minutes. A stopped program applies them when it next starts. Nothing in either file is bound at start, so no field says "restart needed"; the data folder (`RETASKABLE_DATA_DIR`) is environment-only.
- **The shared page host and a page of the CI runner's own** (**[Decided 2026-11-16]**). Everything in 4.2 to 4.4 that isn't the code agent's own (sessions, the one-time token, the password check and its rate limit, the CSRF, Host and Origin checks, the 16 KB limit, the page's CSP, and the two programs routes above) is `src/page_host/`, a neutral module: the code agent's `SettingsServer` builds on it, and so does `retaskable-ci` when it runs on a computer with no code agent (agent-ci.md section 3.9), serving `assets/settings-ci.html` with the programs panel and the CI runner's and Chat Agent's cards. The only difference the host asks of a program is where network access and its password are kept (`page_host::Access`: `machine.json` or `RETASKABLE_NETWORK_PASSWORD` for the code agent, `ci/settings.json` for the CI runner) and whether a password also signs in from this computer while network access is off (no for the code agent, yes for the CI runner, which has no tray link). The look and the page's helpers are `assets/settings-base.css` and `assets/settings-rt.js`, shared by both pages.
- **The page.** The Now tab gets a **Programs** block (state, last seen, version, last error, what the program reports); the Settings tab gets a card for the Chat Agent and for the CI runner (`assets/settings-programs.js`, a second script of the page under the same nonce, which shows everything as text).

## 5. Setup

`retaskable link` (`link_command`), once linked to retaskable.com and with someone at a terminal, asks each in turn with what it is now in brackets, which Enter keeps (`src/setup_questions.rs`): the port, the local model (on or off, then its model and address), how often to check for work, the tags ("none" for none), Cursor's model per tier ("auto" for auto), and network access with its password (not echoed, asked again when too short; on without one stays off). A setting a variable sets is said, not asked. Without a terminal nothing is asked. It saves as the page does (the same `Saver` in `src/settings_server/save.rs`, so the same validation and refusals, asked again) and says "Saved. If Retaskable is running, restart it to use them." A running agent's own saves of machine.json keep the tags and network access setup wrote meanwhile.

## 6. Tested

The agent part (changed 2026-11-09: the agent no longer writes the owner's settings): `agent/src/commands.test.ts` (the wire takes none of the owner's fields; the old `computer_tags`, `agent_settings` and computer-only fields refused), `agent/src/siteSettings.test.ts` (neither schema carrying what's set at the computer).

Secrets (this Rust agent's own design, section 3.3): `src/secrets/tests.rs` (seal and open; a wrong key, a changed ciphertext, another name's, junk and a locked store reading as unreadable; a fresh nonce per save; the environment winning and locked, an empty variable as none, a restart telling saved from environment's; apply and `RETASKABLE_SAVED_SECRETS`; `gh auth setup-git` for `GH_TOKEN` only; the key from the OS store, a file (0600, made on the first save, refused when others can read it) or the variable; no store and no key; the move from the TypeScript agent's entries and file; no value or key in a log line), `src/settings_server/secrets_tests.rs` (the states without values, the planted value found in no response nor in `config.json`, a refused mixed save writing nothing, a save from another computer refused, the published-port rule, no key saying so).

The computer part:

`agent/src/computerStatus.test.ts` (lanes across two projects, an engine's own lane, sessions joined to their lane, a Codex limit and Claude's usage, signed out and budget alerts, the five-hour alert), `agent/src/settingsServer.test.ts` (the status needs a session, then; a missing, wrong, used or old token; the cookie's flags; a wrong Host, Origin or CSRF token, or no JSON; network access off, a tray token from another computer, the password, and the wait after five wrong tries), `agent/src/machineSettings.test.ts` (each variable over the file and reported, what the agent runs on, the checks, saving, the hash, a running agent's save keeping setup's), `agent/src/setupQuestions.test.ts` (answers saved, Enter keeping everything, variables said not asked, network access without a password), `agent/src/tray.test.ts` and `terminalMenu.test.ts` (Settings… and its link with a new token each time).
