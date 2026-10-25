## 3. The bundle (agent-bundle)

One folder that runs the agent with nothing else installed but the tools it drives (section 3.4). Built for `win-x64`, `mac-arm64`, `mac-x64` and `linux-x64` (`linux-arm64` later).

### 3.1 What's in it

- **The agent's executable** (`retaskable.exe` on Windows, `retaskable` elsewhere): Node itself, built as a Node single executable application (SEA) with the agent's code inside. Node's version is `.node-version`'s.
- **The agent's code**, bundled into one JavaScript file by esbuild at build time: no `tsx`, no TypeScript, no `node_modules` at run time.
- **Native pieces beside it**: `@napi-rs/keyring`'s `.node` file and `systray2`'s prebuilt tray helper, each for its OS.
- **No `sharp`.** It draws only the tray icons (`agent/src/trayIcons.ts`) and gallery pictures (`agent/src/galleries.ts`). The icons are drawn at build time and shipped as files (`.ico` on Windows, `.png` elsewhere); galleries go through a step of their own (section 3.5).
- **`build.json`**: the OS, the environment (`production` or `staging`), the version and the commit. In the desktop app it also has `shell`, `"electron"` ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.10): `parseBuildInfo` refuses any other value, and a desktop build's `build.json` without it. The agent reads it at start; with it, the agent is a **release build** (tray.md section 8): its `cloud.url` and environment are fixed and **Link to ▸** offers only that environment.

### 3.2 No terminal window

How each OS starts it without one:

- **Windows**: the executable's PE header is set to the Windows subsystem rather than the console one, so Windows never opens a console for it.
- **macOS**: it lives in `Retaskable.app/Contents/MacOS/` and is started by the app (Finder, Launchpad, login), never by Terminal. `Info.plist` sets `LSUIElement`, so it has a menu-bar icon and no Dock icon or window.
- **Linux**: started from its desktop entry (`Terminal=false`) or XDG autostart, never from a terminal.

And on every OS:

- **Nothing it starts may open a window.** Every `spawn` and `execFile` in `agent/src` passes `windowsHide`: `true`, or `false` in the two places a person asked for a window (`lanePlay.ts`'s Play and `galleries.ts`'s renderer). `engines/cursor.ts`'s `create-chat` was the one without it, fixed in slice 1. A lint test (`agent/src/windowsHide.test.ts`) keeps it so: any call to `spawn`, `execFile`, `exec`, `fork` or their `Sync` forms imported from `node:child_process` in `agent/src` without `windowsHide` fails. (It matters only on Windows; elsewhere it's ignored.)
- **Output goes to a log file** (section 3.3), `retaskable.log`, rotated at 10 MB with 3 old ones kept (`.1` to `.3`). With stdout not a terminal, the terminal menu is never drawn (tray.md section 4.7) and `console.log` goes to the file.
- **A crash before the tray is up** still has to be seen: the launcher shows a dialog ("Retaskable couldn't start: <reason>. The log is at <path>.") instead of failing silently: a message box on Windows, `osascript`'s `display dialog` on macOS, `zenity` or `kdialog` on Linux when either is there, else `notify-send`.
- **The tray's menu gets "Open the log"** (opens the log in the default editor), since there is no terminal to read.
- **Quit** is the tray's Quit or Quit when idle, as now. There is no window to close.

### 3.3 Where its files go

A checkout keeps `.retaskable/` in the repo. A release build keeps it in the OS's own place, `Retaskable` for Production and `Retaskable Staging` for Staging (`retaskable` and `retaskable-staging` on Linux):

| | Data (`machine.json`, each project's `state.json`, `runs/`) | Logs |
|---|---|---|
| Windows | `%LOCALAPPDATA%\Retaskable\` | `%LOCALAPPDATA%\Retaskable\logs\` |
| macOS | `~/Library/Application Support/Retaskable/` | `~/Library/Logs/Retaskable/` |
| Linux | `$XDG_DATA_HOME/retaskable/` (`~/.local/share/retaskable/`) | `$XDG_STATE_HOME/retaskable/logs/` (`~/.local/state/retaskable/logs/`) |

Project checkouts and lane worktrees stay where the user's projects are, as now (agent.md section 4.1).

### 3.4 The tools it drives

The bundle carries Node only. Git, GitHub's `gh` and Claude Code (or `cursor-agent`) stay the user's own installs, signed in as the user: the agent's sessions must act as them (CLAUDE.md). The first-run check (first-run-check.md) already checks each one and says what to do; this spec adds to its fixes:

- **[Proposal]** A missing tool's fix is one button where the OS allows it, then the check runs again:
  - **Windows**: **Install** runs `winget install` for it (`Git.Git`, `GitHub.cli`, and Claude Code's package id, to be checked when built), with `windowsHide`.
  - **macOS**: **Install** for git runs `xcode-select --install` (macOS's own dialog); for `gh` and Claude Code, Homebrew's `brew install` when Homebrew is there, else each tool's own installer opened in the browser.
  - **Linux**: installing needs `sudo`, so the fix shows the one command for the distro it finds (`apt`, `dnf`, `pacman`) with **Copy**, and the agent never runs `sudo` itself.
- Signing in stays the user's own step (`gh auth login`, `claude` then `/login`), opened in a terminal the user can see, since it asks them for something.
- Changed (2026-10-20, [setup-in-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/setup-in-app.md) sections 5 and 6, items install-tools and tool-sign-in): **Install** and **Sign in** become buttons on Getting started, each a request allowed at the computer, with no terminal and no admin password; that spec's installs replace the `winget` and Homebrew proposals above. **Install** is built ([setup-in-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/setup-in-app.md) section 5.0): the GitHub CLI's pinned release into the data folder's `gh/`, Claude Code by Anthropic's own installer, and git by macOS's dialog or Retaskable's own MinGit on Windows; Linux's git shows its command with **Copy**.

### 3.5 What a checkout had that a bundle hasn't

The agent today can lean on being inside this repo. Everything it reads at run time from outside `agent/src` and `shared/` (listed 2026-10-18, slice 1), and what the bundle does with each:

| What | Read by | In the bundle |
|---|---|---|
| `prompts/*.md` and `scouts.json` (`PROMPTS_DIR`) | `prompts.ts`, `review.ts` (through `promptFile`) | Bundled: esbuild inlines them as text (built). A checkout reads them once, as it starts. |
| `agent/package.json`'s version | `agentVersion` (`release.ts`) | Not read: a release build's version is `build.json`'s (built). |
| `systray2`'s `traybin/` helper | `trayHelper.ts`, through `require.resolve` | Shipped beside the executable, in its `node_modules/`, and found there by the same `require.resolve` (built). |
| `@napi-rs/keyring`'s `.node` | `credentials.ts` | Shipped beside the executable, in its `node_modules/` (built). |
| `sharp` and its native binary | `trayIcons.ts` (icons drawn from SVG paths inline in it, not from `app/public/logo.svg`), `galleries.ts` | Icons drawn at build time (built); galleries below. |
| The checkout itself (`ROOT` as a git repo) | `selfUpdate.ts`, `orchestrator.ts`'s self-hosting check, `phone.ts`'s self-hosted build | Off in a release build: it has no checkout (section 3.6). |
| `app/dist` (`APP_DIST`) | `localSite.ts`, only for `npm run launch` | Not in the bundle: a developer's tool. |

Galleries and `lanePlay` read only the project's own files. Galleries' pictures, the one other user of `sharp`, are drawn by the project's own toolchain or ship `sharp`'s prebuilt binary for the OS beside the executable, whichever is smaller when the bundle is built.

### 3.6 Updating itself (agent-self-update)

A release build has no checkout to move forward, so agent.md section 4's self-update is off in it. Every release build updates itself the same way:

- **It asks** its environment's `GET /api/agent/release?os=<os>&arch=<arch>` (a new route: `{ version, url, sha256 }`) at start and every hour: production's for Production builds, staging's for Staging builds.
- **A newer version** is downloaded into the data folder's `updates/`, its hash checked, and swapped in at a restart, the same way a merge restarts the agent today (exit code 75, restart-progress.md): the launcher moves the new folder (or `.app`, or AppImage) in and starts it.
- **On Windows and macOS** the download must carry the same signature as the running app (the same Trusted Signing identity, the same Developer ID), or it is refused.
- **On Windows** the update is the next installer, run silently (`/VERYSILENT`, per user, no admin), which replaces the files while the agent is stopped.
- **The tray's status line** names the version: "Retaskable 1.4.2", or "Retaskable 1.4.2 · Staging".

Sections 3.11 and 3.12.

### 3.7 Details (agent-bundle)

`agent/src/release.ts`, with nothing changed for a checkout:

- **A release build** is one running as Node's single executable application (`node:sea`'s `isSea()`) with `build.json` beside it (`{ os, environment, version, commit }`, environment `production` or `staging`). `BUILD` is that file, or null in a checkout or under plain `node`; a release build whose `build.json` is missing or wrong fails to start.
- **Its folders** are section 3.3's (`releaseFolders`): `DATA_DIR` (`paths.ts`) is the data folder in a release build, `.retaskable/` in a checkout.
- **`--version`** prints the version and stops, in a checkout too: `build.json`'s in a release build, else `agent/package.json`'s (`agentVersion`, which the link and the health check also use).
- **The start** (`releaseStart.ts`, index.ts's first import, so it runs before anything else logs): in a release build `console.log`, `info`, `debug`, `warn` and `error` go to `retaskable.log` in the logs folder, each line timestamped, the first naming the build ("Retaskable Staging 1.4.2 (abc1234) on win-x64"); an uncaught error is logged with its stack.
- **The dialog**: any exit but 0 (quit) or 75 (restart) before the tray is up (`markUp`, once the tray helper has started) shows "Retaskable couldn't start: <the last error logged>. The log is at <path>.", and the exit waits for it to be closed. Windows: PowerShell's WPF message box, hidden console, the text in the environment; macOS: `osascript`'s `display dialog`, the text an argument; Linux: the first of `zenity`, `kdialog` and `notify-send` that runs.

### 3.8 Details (agent-bundle)

What a release build does differently once it runs, with nothing changed for a checkout:

- **A bad `build.json`**: `loadBuild` (`release.ts`) catches it, so `BUILD` is null and `BUILD_PROBLEM` says why ("its build.json, beside <the executable>, couldn't be read (<the error>)"). `releaseStart.ts` shows it in the dialog before anything else runs, without "The log is at" (with no environment there's no log folder yet), and exits 1.
- **One environment** (tray.md decision 7): `cloudUrl` and `cloudUrls` (`link.ts`) give only `build.json`'s environment's URL, ignoring `cloud.url` and `RETASKABLE_CLOUD_URL`, so **Link to ▸** has the one entry, the one connector, and This computer's settings name it. `retaskable link <environment>` (`cli.ts`) refuses any other environment.
- **No self-update**: `Orchestrator.selfHosted` is false, so nothing moves a checkout forward (agent.md section 4.2's update after a merge, the catch-up after a fetch, Play main, Ask Claude's restart) and `restart` only restarts.
- **The restart** (exit code 75, a `restart` command): with no `scripts/supervise.mjs`, `index.ts`'s exit starts the next copy itself (`relaunch`: the same executable and arguments, detached, no window) after the tray helper, connections and port are closed; the new one waits for the port as after any restart.
- **Open the log** in the tray's menu, above Quit (key `open_log`, hidden in a checkout, whose log is its terminal; its tooltip is the path): opens `retaskable.log` in the OS's default app for it, without a shell (`trayAgent.ts` `openFile`: `explorer.exe <file>` on Windows, `open -t` on macOS, `xdg-open` on Linux). The terminal menu leaves it out, as it does any hidden item.

### 3.9 Details (agent-bundle)

`npm run bundle -- [--target win-x64|mac-arm64|mac-x64|linux-x64] [--environment staging|production] [--out <folder>]` (`scripts/bundle.ts`) builds the folder, by default this machine's target and Staging, into `dist/agent/<environment>-<target>/`. Any target builds on any OS, with Node `.node-version`'s major:

- **The code**: esbuild bundles `agent/src/index.ts` into one CommonJS file (Node 22's single executable applications run only CommonJS), with every file in `prompts/` inlined as `RETASKABLE_PROMPTS`, which `promptFile` (`prompts.ts`) reads instead of the folder (`scouts.json` included, which `review.ts` now reads through it). `@napi-rs/keyring` and `sharp` stay outside it. A banner makes `require` one made from the executable's path, since a single executable application's own loads only Node's built-in modules, and `import.meta.url` a file beside it.
- **The executable** (`retaskable.exe`, or `retaskable`): Node's own, at the building Node's exact version (this Node's when the target is this machine's, else nodejs.org's download for it, cached in `node_modules/.cache/retaskable-bundle/`), with the code injected by `postject` (`useCodeCache` and `useSnapshot` off, so the blob runs on any OS). On Windows its PE header's subsystem is then set to Windows (2) from console (3) (`setWindowsSubsystem`), so no console ever opens; its Authenticode signature is stale (`postject` warns) until windows-release signs it. On macOS it's signed ad hoc when built on a Mac; built elsewhere it's left unsigned, with a warning, and macOS won't run it.
- **Beside it**: `build.json` (`{ os: <target>, environment, version: agent/package.json's, commit: the short HEAD }`); `node_modules/@napi-rs/keyring/` (its `package.json` and `.js`) and `node_modules/@napi-rs/keyring-<platform>/` (installed here, or npm's tarball at the version `@napi-rs/keyring` pins); `node_modules/systray2/` (its `package.json` and the one `traybin/` helper); and `icons/<state>.ico` (Windows) or `.png`, drawn now by `drawIcon` (`trayIcons.ts`), which `trayIcon` reads in a release build instead of loading sharp. 87 MB on Windows.
- **Checked**: the build starts this machine's target with `--version` and fails unless it prints `build.json`'s version.
- **No sharp**: it is no longer loaded at start. `trayIcons.ts` and `galleries.ts` import it only when they draw; in a bundle, which doesn't carry it, a gallery's shoot fails with "Galleries need sharp, which this build doesn't have (…)." until section 3.5's galleries step.

Tried by hand (2026-10-18): the Windows build answers `--version` and, its data folder pointed at a scratch one holding a bad `config.json`, loads every module, logs its build line and the error to `retaskable.log` and exits 1; the Linux build, made on Windows, does the same in `node:22-bookworm`.

### 3.10 Details (agent-bundle)

- **The start check**: `npm run bundle`, after `--version`, starts this machine's target as an installed agent is started (`startCheck`, `scripts/bundle.ts`): its output piped, not a terminal; `HOME`, `USERPROFILE`, `LOCALAPPDATA`, `XDG_DATA_HOME` and `XDG_STATE_HOME` in a scratch folder, so its folders are new; `RETASKABLE_NO_TRAY` (a build machine may have no desktop), a free `RETASKABLE_PORT`, and `RETASKABLE_CREDENTIALS_FILE` in the scratch folder, so its secrets go to a file there and never to the computer's credential store (`credentialEntry` in `agent/src/credentials.ts`). It fails unless, within 60 seconds, `GET /api/health` answers `build.json`'s version, the log has the agent's `Links:` line saying its environment is **not linked**, and nothing was written to the terminal. Then it's stopped. A not-linked agent opens no connection, so the check needs no cloud, fake or real. Until 2026-10-18 it used the computer's own credential store, which a scratch home doesn't keep apart: on a developer's machine linked to that environment it said linked, and it had refreshed that link, spending the refresh token, before it was stopped without keeping the new one. The computer's own agent sent the spent token at its next restart, and the cloud, reading a reuse as a stolen token, deleted it (staging's agent 1, unlinked at 21:50 that day after a lane ran `npm run bundle` at 21:41 and 21:42).
- **The `Links:` line**: every agent, checkout or release build, logs one at start, after its connections start: "Links: staging.retaskable.com not linked, backlog store backlog.retaskable.com not linked" (`linksLine`, `computerView.ts`, the same words as This computer's settings), so a release build's log says whether it's linked.
- **On Windows** the build reads the subsystem back from the written executable's PE header and fails unless it's Windows (2), whatever OS built it.
- **CI**: `.woodpecker/bundle.yaml` (`node:22-bookworm`) runs `npm run bundle` for win-x64, mac-arm64, mac-x64 and linux-x64 on every push to `main` or `dev/**`, and on a pull request that changes how it's built (`scripts/bundle.ts`, `release.ts`, `releaseStart.ts`, `trayIcons.ts`, `prompts.ts`, `.node-version`, `package-lock.json` or the pipeline), under ci.yaml's `ci-ready` rule for Retaskable's own, and only once ci.yaml's checks pass (`depends_on: [ci]`: run beside them on one agent, the checks were killed before they started). linux-x64 is the machine's own target, so it's also started and checked. The macOS executables are left unsigned (built on Linux), so they run only once signed on a Mac: Developer ID for Staging and Production alike (section 6.1, staging-builds and mac-release). A push to `main` then publishes the AppImage to staging (section 3.16).

Tried (2026-10-18): the Windows build's start check passes on a developer's machine linked to Staging, saying not linked, and the machine's own link is untouched; the Linux build, started in `node:22-bookworm` with a scratch home and no tray, answers its health, logs "Links: staging.retaskable.com not linked, …" and writes nothing to its terminal.

agent-bundle is built. Galleries' pictures in a bundle (section 3.5, `sharp` or the project's own toolchain) are a step of their own, not yet an item.

### 3.11 Details (agent-self-update)

Section 3.6's asking and downloading, with nothing changed for a checkout:

- **The feed**: the hub answers `GET /api/agent/release?os=<os>&arch=<arch>` with `{ version, url, sha256 }` for one of the bundle's targets (`win-x64`, `mac-arm64`, `mac-x64`, `linux-x64`). The agent only reads it: an answer that isn't one (a 404, or an entry that isn't a version of dotted numbers, an `https` URL and 64 lowercase hex digits) is "no release for that yet", and the agent stays as it is. What serves the feed, and where its entries are kept, is the hub's.
- **Asking**: a release build's `ReleaseUpdater` (`agent/src/releaseUpdate.ts`, started in `index.ts` after the connections) asks its own environment (`cloudUrl`, `build.json`'s) for `build.json`'s `os`, at start and every hour (`UPDATE_CHECK_EVERY_MS`), one check at a time. A 404, or a version not later than its own (`isNewerVersion`, number by number), does nothing; any other failure logs "Update: couldn't check for a newer version (…)." and the next hour tries again.
- **Downloading**: a later version goes to the data folder's `updates/<version>/<the URL's file name>` (kept to letters, digits, `.`, `-` and `_`), as `.part` until its SHA-256 is the feed's, then renamed; other versions' folders in `updates/` go. A wrong hash deletes the folder and logs "… isn't the build the feed names (its SHA-256 differs), so it was refused". One already there with the right hash isn't fetched again, and the same version isn't asked for twice. Once it's ready the log says "Update: 1.4.3 downloaded and checked, <file>." and `ready` holds it.
- **The tray's status line** starts with `releaseTitle` (`release.ts`) in a release build, "Retaskable 1.4.2 · Staging · idle" ("Retaskable 1.4.2 · idle" for Production), and "Retaskable · idle" in a checkout as before.

Swapping it in and the signature check are slice 2 (section 3.12).

### 3.12 Details (agent-self-update)

Installing the download, with nothing changed for a checkout. `installerFor(build)` (`agent/src/releaseInstall.ts`) is how a copy replaces itself, and what each package must be:

| | The feed's file | Readied (`prepare`, after the hash) | Installed (`install`, at the restart's exit) |
|---|---|---|---|
| Windows | The next installer (`.exe`) | Its Authenticode signature compared with the running `retaskable.exe`'s (PowerShell's `Get-AuthenticodeSignature`, the path in the environment) | Run detached with `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`; the installer waits for the agent to stop, replaces the files and starts it again, even when silent (windows-release's and staging-builds' installer must) |
| macOS | The `.app`, zipped | Unpacked by `ditto -x -k` into `updates/<version>/app/` (one `.app` in it, else refused); its Team ID (`codesign -dv`) and `codesign --verify --deep --strict` compared with the running `.app`'s | The running `.app` renamed to `.app.old`, the new one moved in (copied when on another volume), the old one deleted, and its executable started with the same arguments |
| Linux | The AppImage | Made executable; an AppImage has no signature, so the hash is its check | Copied beside the running one (`$APPIMAGE`) as `.new`, renamed over it, and started |

- **The same signature** (`signatureRefusal`): a running app that is validly signed refuses a download that isn't, or is signed by anyone else ("the download was refused: it's signed by …, not … as this copy is"); the folder is deleted, no restart is asked for, and the next hour asks again. A running app that isn't validly signed (every build until windows-release and mac-release sign them; the Windows executable's Authenticode is stale from `postject`) has nothing to compare with, so the feed's SHA-256 is its only check.
- **A copy that can't replace itself** (`cannot`): on macOS one not running from an `.app`, on Linux one not running as an AppImage (no `$APPIMAGE`), such as `npm run bundle`'s loose folder. It downloads nothing and logs once "Update: 1.4.3 is out, but this copy can't replace itself (it isn't running as an AppImage)."
- **The restart**: once ready, `onReady` asks for a restart with `reason: "release"` and the version ([restart-progress.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/restart-progress.md) section 3.1), which goes now, what's running picking up again after (agent.md section 4.2), so the Agents tab says "Restarting · to install 1.4.3 at 14:02 · finishing a merge first" while one is under way, then "Restarting…", then "Restarted at 14:05 · to install 1.4.3 at 14:02" (`restart.json` carries the version). At the exit (code 75, whatever asked for the restart) `ReleaseUpdater.installNow` installs the ready build instead of `relaunch`; with none ready, or an install that throws (logged "couldn't install 1.4.3 (…), so this copy starts again"), it relaunches itself as before.
- **No loop**: just before installing it leaves `updates/<version>/tried`. A copy that starts again still older than that version (the install didn't take) doesn't download it or restart for it again, and logs once that it isn't tried again until a newer one is out; a newer version's download removes the folder.

Staging's AppImage is published into `AGENT_RELEASES` by each merge to `main` (section 3.16); the macOS and Windows builds are the desktop app's ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.7).

### 3.13 Details (staging-builds)

Section 8.1's Staging and Production side by side, with nothing changed for a checkout or for Production:

- **Port**: with neither `RETASKABLE_PORT` nor the config's `port`, a Staging release build's health check is on 4701 (`defaultPort`, `release.ts`), Production's and a checkout's on 4700.
- **Name**: `appName` (`release.ts`) is "Retaskable Staging" in a Staging release build, else "Retaskable". It starts the tray's tooltip ("Retaskable Staging · Staging: connected"), the notifications' titles ("Retaskable Staging: waiting for you", "Retaskable Staging: approve the shared backlog"), and the crash dialog's title and text ("Retaskable Staging couldn't start: …"; a bad `build.json` still says "Retaskable", having no environment). The status line was already `releaseTitle`'s, and the folders section 3.3's.
- **Icon**: `iconSvg(state, environment)` (`trayIcons.ts`) puts a dark tile with a white "S" at the top right of Staging's icons, drawn as a path (no font); `npm run bundle` draws `icons/` with the build's environment, so only a Staging bundle carries it.

### 3.14 Details (staging-builds)

Start at login (sections 5.1, 6.1 and 6.2) for every release build, Production's and Staging's apart, with nothing changed for a checkout. `agent/src/startAtLogin.ts`:

- **The entry** (`loginEntry`), naming what starts this copy (`process.execPath`; on Linux `$APPIMAGE` when it runs as one, not the executable inside its mount):

  | | Production | Staging |
  |---|---|---|
  | Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`'s `Retaskable` value, the executable's path quoted | its `Retaskable Staging` value |
  | macOS | `~/Library/LaunchAgents/com.retaskable.agent.plist` (`Label`, `ProgramArguments` the executable, `RunAtLoad`, `LimitLoadToSessionType` `Aqua`) | `com.retaskable.agent.staging.plist` |
  | Linux | `$XDG_CONFIG_HOME/autostart/retaskable.desktop` (`~/.config/autostart/`; `Name`, `Exec` quoted as the desktop entry spec asks, `Terminal=false`) | `retaskable-staging.desktop` |

- **Reading and setting it** (`StartAtLogin`): a file's there or not; on Windows `reg.exe query`, `add … /t REG_SZ /d <path> /f` and `delete … /f`, with no window and no shell, a failure thrown with its exit code. What it read or set is kept, so the tray's refresh doesn't start `reg.exe` every 2 seconds.
- **At start** (`atStart`, `index.ts`): on the first run (no `start-at-login` file in the data folder, which it then writes) macOS and Linux turn it on, by default as section 6.1 says; on Windows the installer's page decides (section 5.1), so the first run leaves the Run key as it is. After that an entry that's on is written again, so it names where this copy is now; one that's off stays off. A failure is logged ("Couldn't set up start at login: …") and the agent carries on.
- **The tray's tick** (key `start_at_login`, after Pause this computer, hidden in a checkout): **Start with Windows** on Windows, **Start at login** elsewhere, ticked while on (the helper's `checked`; the terminal menu shows `[x]` or `[ ]`); a click turns it the other way and logs "Start at login: off". One that can't be read is shown off.

### 3.15 Details (staging-builds)

`npm run package -- [--target mac-arm64|mac-x64|linux-x64] [--environment staging|production] [--bundle <folder>] [--out <folder>]` (`scripts/package.ts`) makes a bundle (`npm run bundle`'s `dist/agent/<environment>-<target>/` by default) into the package its OS installs and section 3.12 replaces, in `dist/agent/packages/`, and prints its SHA-256 (what its feed entry will name). Nothing is signed:

- **macOS**: `Retaskable Staging.app` (`Retaskable.app` for Production) zipped as `Retaskable-Staging-mac-arm64.zip` (`-mac-x64`; `Retaskable-mac-arm64.zip`), the zip written by the script itself, deflated, each entry carrying its Unix mode so `ditto -x -k` keeps the executables executable. In it: `Contents/Info.plist` (`CFBundleIdentifier` `com.retaskable.agent.staging` or `com.retaskable.agent`, the name, the version, `LSUIElement`, macOS 11 and later) and `PkgInfo`; the executable alone in `Contents/MacOS/`; and the rest of the bundle (`build.json`, `icons/`, `node_modules/`) in `Contents/Resources/`, since only code may be in `Contents/MacOS/` for the app to be signed. `bundleDir` (`release.ts`) is where a release build looks for them: beside the executable, or a `.app`'s `Contents/Resources/`; `build.json`, the tray icons and the bundle's banner (its `require` and `import.meta.url`, so the native pieces) all use it. No `.icns` yet: with no Dock icon, only Finder shows the app's own.
- **Linux**: `Retaskable-Staging-x86_64.AppImage` (`Retaskable-x86_64.AppImage`): AppImage's type 2 runtime (pinned, 20251108, its SHA-256 checked, cached with Node's downloads) followed by a zstd squashfs (`mksquashfs`, squashfs-tools; so it's made on Linux or macOS) of the bundle with `AppRun` a link to `retaskable`, `retaskable-staging.desktop` (`Terminal=false`), its 256-pixel icon (the tray's connected one) and `.DirIcon`. On a linux-x64 machine it's then started as `startCheck` starts a bundle (no terminal, a scratch home, health answering its version, the log's `Links:` line), extracted rather than mounted where there's no FUSE (`APPIMAGE_EXTRACT_AND_RUN`). The runtime runs the agent as its child, so `startCheck` now starts what it checks in its own process group off Windows and stops the whole group.
- **CI**: `.woodpecker/bundle.yaml` installs squashfs-tools and, after the bundles, packages mac-arm64, mac-x64 and linux-x64, starting the AppImage. A merge to `main` then uploads the AppImage (section 3.16).

Tried (2026-10-18): in `node:22-bookworm` with no FUSE, the Staging AppImage (42 MB) starts, answers its health and logs "not linked", and nothing is left running once it's stopped; `Retaskable-Staging-mac-arm64.zip` (40 MB) is made there too, not yet opened on a Mac. The Windows bundle, with the banner's new `bundleDir`, still passes its start check.

### 3.16 Details (staging-builds)

Each merge to `main` publishes staging's AppImage, so a staging Linux release build updates itself (section 3.11) within the hour:

- **Its version**: `npm run bundle -- --version <n.n.n>` writes that into `build.json` instead of `agent/package.json`'s (dotted numbers, else refused). CI passes `agent/package.json`'s major and minor with Woodpecker's pipeline number (`0.1.812`), so every build is later than the one before it, whatever the branch.
- **Publishing** is the hub's and the build's, not the agent's: the hub serves the packages and the feed (`GET /api/agent/download/<version>/<file>`, and the `GET /api/agent/release` above), and CI uploads each build to them with `npm run upload-release`. The agent asks only for the feed entry and downloads the file it names.

Since 2026-10-20 the macOS and Windows parts of what was planned next are desktop-mac's and desktop-windows' ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md)): the Electron app is what Staging ships on those two OSes, with its installer, signing and `/download/mac` and `/download/windows` pages, so staging-builds is left with Linux's (section 3.17).

### 3.17 Details (staging-builds)

Linux's last two pieces, with nothing changed for a checkout or for Windows and macOS:

- **The download**: Getting started's **Link this computer** step offers the Linux AppImage from the environment's own feed; that page is the app's, and the agent only has to be an AppImage that runs (section 6.2).
- **The app menu** (`agent/src/appMenu.ts`, called from `index.ts` after start at login): a release build running as an AppImage (`$APPIMAGE` on Linux; `menuEntry` is null otherwise) writes `$XDG_DATA_HOME/applications/retaskable-staging.desktop` (`~/.local/share/applications/`; `retaskable.desktop` for Production) on its first run, remembered by `app-menu` in the data folder: the same entry as the AppImage's own (`desktopEntry`, which `scripts/package.ts` now uses too) with `Exec` the AppImage's path, quoted as start at login's is, and `Icon=retaskable-staging`, its 256-pixel icon copied from the AppImage's root (`$APPDIR`) to `icons/hicolor/256x256/apps/` beside it (left out when it isn't there). After that, an entry that's there is written again, so it names where the AppImage is now; one the user removed stays removed. A failure is logged ("Couldn't add Retaskable to the app menu: …") and the agent carries on.

### 3.18 Details (linux-release)

Production's AppImage (section 6.2), published by each deploy to production, with nothing changed for staging or a checkout:

- **CI**: `.woodpecker/release.yaml`, on a push to the `production` branch only (a deploy to production, [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 9), runs in `node:22-bookworm` with squashfs-tools: `npm run bundle -- --target linux-x64 --environment production --version <major.minor>.<pipeline number>` (the same version as bundle.yaml's, so each is later than the last), `npm run package -- --target linux-x64 --environment production` (which starts `Retaskable-x86_64.AppImage` as section 3.15 does), then `npm run upload-release -- --environment production`, into `retaskable-agent-releases-production` (made by `deploy:production`'s `bucket:production`), with the same `cloudflare_r2_token` and `cloudflare_account_id` Woodpecker secrets as staging's publish. It is its own workflow, with no `depends_on: [ci]`: ci.yaml doesn't run on `production`, whose commits passed it on `main`, and Woodpecker drops a workflow whose dependency isn't in the pipeline.
- **Its folders, name, port, start at login and app-menu entry** are Production's already (sections 3.3, 3.13, 3.14 and 3.17): `~/.local/share/retaskable/`, "Retaskable", 4700, `~/.config/autostart/retaskable.desktop` and `~/.local/share/applications/retaskable.desktop`.

Not yet tried by hand on Ubuntu LTS and Fedora, on KDE and on GNOME with and without the AppIndicator extension (section 9).

