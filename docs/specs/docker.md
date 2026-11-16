# The Agent in Docker — Spec

Status: v0.1. Builds on [agent.md](agent.md) sections 4 and 6, [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 4.2 (linking a machine), [tray.md](tray.md) section 4.7 (the terminal menu) and [ci-link.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-link.md).

Theme: Orchestration

Kind: new

## 1. What it is

The same agent, run in a Linux container on a Windows machine with Docker Desktop (WSL2), instead of on Windows itself. Nothing in the agent changes for it beyond section 4 and the machine's name (section 3.4).

Why: on Windows every new process is slow (Defender scans each one, and without admin it can't be told not to). Measured on the operator's machine: `node -e 0` 272 ms on Windows against 15 ms in a container, `git --version` 76 ms against 0.6 ms. The agent and its checks start thousands of processes, so `npm test` takes about 2 minutes on Windows and about 20 seconds in the container (section 6).

The files are in `docker/agent/`: `Dockerfile`, `compose.yaml`, `start.sh` and a `README.md` for the operator.

## 2. The image

- **Base:** `node:22-bookworm-slim`, Node's major version from `.node-version`.
- **Tools:** git, `gh` (GitHub's apt repository), `glab` (GitLab's release `.deb`, version pinned by the `GLAB_VERSION` build argument), the Claude Code CLI (`npm i -g @anthropic-ai/claude-code`), python3, `tini` as PID 1, and Playwright's Chromium with its OS libraries at the version in `package-lock.json` (`PLAYWRIGHT_VERSION`, 1.63.0), installed in `/ms-playwright` for every user. Cursor's agent isn't installed: a project that picks Cursor needs it added.
- **User:** `node` (uid 1000), not root. Claude Code's own updater is off (`DISABLE_AUTOUPDATER`): its install is root's, and a new one comes with a rebuild.
- **git** trusts every folder (`safe.directory '*'`, system-wide): the container holds only its own volumes, and bookworm's git has no `/work/*` pattern.
- **Nothing secret** is in the image: no token, no login, no checkout.
- **No GPU.** The operator's Docker can pass one through, but nothing here uses it and the compose file doesn't ask for it.

In this Rust agent, `docker/agent/` (`Dockerfile`, `compose.yaml`, `start.sh`, `README.md`) is the port, built from the repo root with `docker compose -f docker/agent/compose.yaml up -d --build`. The image is still `node:22-bookworm-slim` with the same tool list (git, `gh`, `glab` pinned by `GLAB_VERSION`, `python3`, `tini`, Playwright's Chromium pinned by `PLAYWRIGHT_VERSION`, the Claude Code CLI pinned by `CLAUDE_CODE_VERSION` with `DISABLE_AUTOUPDATER` set, Cursor's agent left out) and the same `node` uid-1000 user, `safe.directory '*'` and no-secret rule — Node stays the base image because the Claude Code CLI and the projects the agent builds and tests still need it, even though the agent itself no longer does. The difference is the agent's own executable: a multi-stage build cross-compiles `retaskable` (`cargo build --release --bin retaskable`, with `assets/settings.html` copied in for its `include_str!`) and bakes the binary straight into the image, instead of cloning a checkout and running it from source. Before switching to `node`, the image also creates and hands over the volumes' mount points (`/work`, `/home/node/.claude`, `/home/node/.config/gh`), since Docker would otherwise create a fresh named volume's mount point as root.

## 3. How it runs

### 3.1 Volumes

Named Docker volumes, never a Windows folder mounted in (a bind mount is slow and scanned):

| Volume | Mounted at | Holds |
|---|---|---|
| `work` | `/work` | Retaskable's checkout (`/work/Retaskable.com`), which the agent updates itself in; its `.retaskable/` data, with `credentials.json` (section 4); every project's checkout, cloned beside it, and their lanes' worktrees (`/work/.retaskable-worktrees/<project>`, agent.md section 4.1) |
| `claude` | `/home/node/.claude` | Claude Code's settings and sign-in (`CLAUDE_CONFIG_DIR` points there, so `.claude.json` is kept too) |
| `gh` | `/home/node/.config/gh` | `gh`'s settings |

Rebuilding the image or recreating the container keeps all three.

In this Rust agent, the volumes are four: a `data` volume at `/data` is the data folder (`RETASKABLE_DATA_DIR=/data`: `config.json` with the saved secrets, `secrets.key`, `start-paused.json`, link credentials, events), and `work` holds only each linked project's checkout and lane worktrees — there's no Retaskable.com checkout to keep beside them, since the agent itself is the image's baked-in binary, not something cloned into `/work`. The image makes `/data` and gives it to `node`, so a fresh volume is writable.

### 3.2 First run

`start.sh` is the container's command. Each start it: runs `gh auth setup-git` when `GH_TOKEN` is set (git over https then signs in with it); sets git's `user.name` and `user.email` from `GIT_USER_NAME` and `GIT_USER_EMAIL`; clones `https://github.com/Retaskable/Retaskable.com` (`RETASKABLE_REPO_URL` overrides it) into `/work/Retaskable.com` when there's no checkout yet; runs `npm ci` when `node_modules/` or `app/node_modules/` is missing; then runs `node agent/scripts/supervise.mjs` with its own arguments, which starts the agent again when it restarts itself (exit code 75, agent.md section 7). After that the agent's self-update keeps the checkout and its packages current, as on any machine.

In this Rust agent, `start.sh` only sets git's `user.name` and `user.email` and then `exec`s `retaskable`; it runs no `gh auth setup-git` and reads no token, since the agent does that itself for a saved `GH_TOKEN` (`Secrets::apply`, agent-settings.md section 3.3) — there's no checkout to clone or `npm ci` to run first, and no supervise loop: this port has no release-bundle self-update yet (`installers/03-the-bundle-agent-bundle.md` section 3.12 isn't built), so getting a newer agent means rebuilding this image, not something a running container fetches for itself.

### 3.3 Credentials by environment

Set in the shell that runs `docker compose`, or a `.env` file beside `compose.yaml` that is never committed:

- `GH_TOKEN` (required): `gh`'s login and git's over https.
- `CLAUDE_CODE_OAUTH_TOKEN`: a long-lived Claude Code token from `claude setup-token` (a Claude subscription's). `claude auth status` counts it as signed in, so the first-run check (first-run-check.md) does too. Or leave it unset and sign in once inside the container (`docker exec -it retaskable-agent claude`, then `/login`), or copy a `.credentials.json` into the `claude` volume; either is kept in the volume.
- `RETASKABLE_WOODPECKER_TOKEN` (optional): Woodpecker's logs for failed checks, as on Windows.
- `GITLAB_TOKEN` (optional): `glab`, for GitLab projects.
- `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` (optional): the computer that runs retaskable.com's rollouts, for the `traffic` and `health` commands ([rollout-on-server.md](rollout-on-server.md) section 3, slice 3). Other projects leave them empty.

The agent's sessions inherit its environment, so these reach `claude -p`, `gh` and the checks, as the same logins do on Windows through the OS store and the user's environment.

In this Rust agent, no secret is passed in the environment: the compose file passes none. Each is saved once with `retaskable secret set <NAME>` (agent-settings.md section 3.3: asked for with no echo or piped on stdin, never an argument; sealed under the key in `/data/secrets.key`, which the first save makes), for example `docker compose -f docker/agent/compose.yaml run --rm agent retaskable secret set CLAUDE_CODE_OAUTH_TOKEN` before the first start. At start the agent decrypts the saved ones into its own environment, where `claude -p`, `gh`, `glab` and the checks inherit them, and runs `gh auth setup-git` for a saved `GH_TOKEN`; the compose file runs `RETASKABLE_ORCHESTRATOR=runs` (production), and Claude Code keeps running with `--permission-mode auto`. `RETASKABLE_WOODPECKER_TOKEN` is not one of the saved secrets here and is still passed through from the environment when set.

### 3.4 The machine's name

A container's host name is a random id. `RETASKABLE_MACHINE` names the machine instead, wherever the agent says its name (the `/link` page, the connector's hello, merge locks, tryouts), e.g. `jcnash-docker`; the compose file also sets the container's host name to it. Unset, the agent uses the host name, as before (`machineName`, `agent/src/link.ts`).

In this Rust agent, this was already built for every machine (`machine_name`, `src/link/mod.rs`), not added for this container; the compose file passes `RETASKABLE_MACHINE` through and sets the container's host name to it the same way.

### 3.5 The terminal menu

There's no desktop in the container, so with a terminal the agent draws the tray's menu in it (tray.md section 4.7). The compose file keeps one open (`stdin_open`, `tty`), and `docker attach retaskable-agent` reaches it. **Ctrl-P Ctrl-Q** leaves it running; Ctrl+C there stops the agent, and the container with it (`restart: unless-stopped` starts it again; `docker compose stop` stops it for good). The menu's **Quit** is the same.

In this Rust agent, there's no tray menu yet in any environment, so the compose file doesn't open a `stdin_open`/`tty` terminal for one and `docker attach` has nothing to show. `docker compose logs -f` is how you watch the container today, and `docker compose stop` (or `down`, which also removes the container but keeps the volumes) is how you stop it — the agent already shuts down gracefully on that signal (`serve()`'s `with_graceful_shutdown`, `src/main.rs`).

## 4. Credentials without an OS credential store

The agent keeps its secrets (the refresh token of each link, [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 4.2; the CI tokens and the agent's sealing key, [ci-link.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/ci-link.md); issue trackers' keys, issue-import.md) in the OS credential store through `@napi-rs/keyring`. A container has none: no Secret Service, and Docker's default seccomp profile refuses the kernel keyring, so every read and write throws (`Couldn't access platform storage: PermissionDenied`).

- **The rule** (`agent/src/credentials.ts`): on Linux, the agent reads a probe entry once; if that throws, there's no OS credential store and every secret goes in `.retaskable/credentials.json` instead, by service and account as in the store. Windows and macOS always use the OS store, and so does Linux wherever the probe works (a desktop's Secret Service, or the kernel keyring on a headless box), exactly as before.
- **The file** is written whole to a temp file created with mode 0600 and renamed over, so it's never half written nor readable by another user; a write tightens a looser mode. A file that isn't JSON is an error, never read as empty (a write would wipe the other tokens).
- **At start** the agent says so: "No OS credential store here: link tokens and keys are kept in …/credentials.json, readable by this user only."
- **The trade:** anyone who can read the `work` volume (or run `docker exec` as the user that owns Docker) has the tokens, as with any server's token on its disk. That's the same reach as the checkout, the session transcripts and the `GH_TOKEN` in the container's environment. Forgetting a link from the menu, or unlinking the computer on retaskable.com, revokes it as anywhere else.

In this Rust agent, this rule was already built for every machine (`OsStore`, `FileStore`, `os_store_works`, `credential_file_note`, `src/link/credentials.rs`), not added for this container; it probes the same way on Linux and was confirmed to fall back and log the same banner when run in this container, which has neither a Secret Service nor kernel-keyring access.

In this Rust agent the tokens saved on the settings page are not in `credentials.json` but ciphertext in the config ([agent-settings.md](agent-settings.md) section 3.3). A container has no OS store to hold the key for them, so set `RETASKABLE_SECRETS_KEY_FILE` (the image sets it to `/data/secrets.key` in the `data` volume: a file holding a 32-byte key as base64, mode 0600; the first save makes it if it isn't there) or `RETASKABLE_SECRETS_KEY`; with neither, the page says secrets can't be saved. **The key then sits in the same volume as the config**, so protection there is file permissions, the same reach as the trade above.

## 5. Switching a machine over

1. Build and start it: `docker compose -f docker/agent/compose.yaml up -d --build`, with the section 3.3 variables set.
2. Stop the Windows agent (its tray's **Quit**), so two agents don't claim work for the same projects.
3. `docker attach retaskable-agent`, then **Link to ▸ Production ▸ Link**: the menu says the code and the `/link?code=…` address; open it on Windows, check the machine's name (`RETASKABLE_MACHINE`) and approve. Or `docker exec -it -w /work/Retaskable.com retaskable-agent npm run link`. Once that's approved the agent starts the shared backlog's link by itself: its `/link?code=…` address is in the log and in **Requests** (`npm run link -- backlog` does the same); approve it the same way. The same **Link to** for any other environment.
4. On retaskable.com, Settings → **Link project…** for each project: it's cloned into `/work` beside Retaskable's checkout. Settings → CI → **Link** seals the Woodpecker token to the new computer, unless `RETASKABLE_WOODPECKER_TOKEN` is set.
5. Resume the projects. The Windows computer can be unlinked on retaskable.com once the container has taken over.

In this Rust agent the container is `retaskable-agent-docker`, secrets are saved before step 1's start (section 3.3), it starts paused until step 5 (section 8), and step 3's tray menu doesn't exist yet (section 3.5), so link the computer with `docker exec -it retaskable-agent-docker retaskable link [production|staging|local]` instead: it runs the same device flow in the terminal, printing the code and the `/link?code=…` address to approve the same way. `retaskable link backlog` does the shared backlog's link the same way, though it also starts by itself once the computer already holds a cloud link (`auto_link_backlog`, `src/main.rs`). Step 4's **Link project…** and CI **Link** aren't wired to this port's CLI yet, on this or any other machine — that's a later item, not this one — so until then a project is added by hand, cloned under `/work` and pointed to from `/work/.retaskable/config.json` (`docker/agent/README.md`), the same on every machine running this port.

## 6. Measured

On the operator's machine (2026-10-16), the full `npm test` (lint, 3,318 agent, cloud, backlog, shared and scripts tests, then 898 app tests) in a container with the checkout in a volume: **19 s**; on Windows, the same commit: **126 s**. `npm ci` 18 s against 21 s; `npm run typecheck` 12 s in the container. The image is 1.74 GB: 1 GB of it is Chromium and its libraries, 270 MB Claude Code.

## 7. Reaching the pages from the host

**[Decided 2026-11-16]** The code agent's settings page (4700) and the CI runner's (4702) are reachable from the host's browser, Windows included, and from nowhere else.

- **The publish.** Each compose file publishes its port on the host's `127.0.0.1` only (`"127.0.0.1:4700:4700"`, `"127.0.0.1:4702:4702"`), never on another address. Docker then delivers the host's connection to the container from the container network's default gateway, not from a loopback address, and only a page listening on `0.0.0.0` inside the container is reached at all.
- **The opt-in.** `RETASKABLE_TRUST_DOCKER_HOST=1`, set by both compose files, makes the page (the neutral `src/page_host/`, so the code agent's settings server and the CI runner's `serve` alike) listen on `0.0.0.0` inside the container, read the default route's gateway from `/proc/net/route` once at start (logged), and count a request from that address as this computer: for the one-time sign-in link, the password-optional sign-in and saving secrets (agent-settings.md section 4.3 and 4.4). Another peer is treated exactly as with network access off: refused with the same 404. Host and Origin stay `localhost:<port>` or `127.0.0.1:<port>`. Linux only, and without the variable nothing changes (`127.0.0.1`, loopback only). With no gateway readable nothing is trusted.
- **Why the publish is the boundary.** The container's own network gives the page `0.0.0.0` and nothing else to tell the host from another container, so what makes the gateway mean "the host" is that the port is published on `127.0.0.1` only and no other program is given the gateway's address. The project rule (CLAUDE.md) reads "binds `127.0.0.1`; inside a container started with the Docker opt-in it binds `0.0.0.0` and is published on the host's `127.0.0.1` only".
- **Signing in.** Open `http://localhost:4700/settings` (agent) or `http://localhost:4702/settings` (CI). The one-time link is in each container's log (`docker compose logs`): with the opt-in, the code agent prints one at every start ("Settings page, open it once with: …"), as the CI runner does; `docker restart` prints a new one. The agent's port is `RETASKABLE_PORT` (default 4700), the same inside the container and on the host, since the page checks that Host names its own port: set it (e.g. 4710) when another agent on the computer already has 4700. The session cookie's name carries the port (`retaskable_settings_session_<port>`): a browser keeps cookies by host, not port, so the two pages would otherwise sign each other out. The CI runner's password signs in from the host too (`retaskable-ci set-password`); the code agent's is for the network only, so it is the link that signs in.

## 8. Starting paused

A new computer must not take work before its owner says so. The agent compose file sets `RETASKABLE_START_PAUSED=1` (agent-settings.md section 3.4): each project this computer has not paused this way before starts with this computer's own pause on, recorded in `/data/start-paused.json`, until someone resumes it from the site or the page; a restart after that doesn't pause it again, and a project linked later starts paused too. Starting before the first start with no project yet is fine: nothing is recorded until there is one.
