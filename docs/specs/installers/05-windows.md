## 5. Windows

### 5.1 Production: the installer (desktop-windows, windows-release)

Since 2026-10-20 the installer is electron-builder's NSIS, not Inno Setup, and installs the desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 4) in place of this spec's own Node bundle, zipped folder and update code (section 1.1):

- **`RetaskableSetup.exe`**, from retaskable.com's Getting started (desktop-app.md section 3.8's page, for Windows: 5.4.7). It installs **per user** into `%LOCALAPPDATA%\Programs\Retaskable\`, so there's no admin prompt, adds a Start Menu entry, and opens the app when it finishes. Its only page is "Start Retaskable when I sign in" (ticked) and **Install** (desktop-app.md section 4).
- **Start at login** is Electron's login item (desktop-app.md sections 3.5, 4), which sets the same `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key underneath; the tray's menu keeps **Start with Windows** (a tick) reading and setting it.
- **Uninstall** from Settings › Apps (an uninstall entry under `HKCU`), which stops the agent and removes the program and data folders but not project checkouts (5.4.6).
- **desktop-windows is built**: the app itself — the window, the tray icon, the login item, today's unsigned `RetaskableStagingSetup.exe` — runs on Windows already. **windows-release is what's left**: signing it for both environments, publishing it, and Getting started's download, below (5.4).
- **[Proposal]** An `.msi` (WiX) too, later, only if a company asks for one to deploy with its own tools. The `.exe` is simpler to build and installs per user without admin.

### 5.2 Staging: the same installer (staging-builds, windows-release)

`RetaskableStagingSetup.exe`, from staging's Getting started: the same installer with the Staging bundle, into `%LOCALAPPDATA%\Programs\Retaskable Staging\`, with its own Run key value and uninstall entry (desktop-app.md section 4). **[Proposal]** Today it ships unsigned, since nothing has signed a Windows build yet; windows-release signs **both** environments' installers, Staging's first — not because an unsigned `.exe` refuses to run, as an unsigned Mac one does, but to prove the signing step on the build fewer people are waiting on before it's trusted for Production. Signed with the same identity either way, since the signing is paid for either way and an unsigned exe with Node inside gets quarantined by Defender often enough to matter.

### 5.3 Signing

- **Azure Trusted Signing** (about $10 a month, Basic): no hardware token, signing from CI with a short-lived certificate, and the signature chains to Microsoft's trusted roots.
- **Eligibility** is checked first: Trusted Signing verifies the organisation (or, where offered, the individual) before it issues anything. If Retaskable can't qualify, an OV certificate from a commercial CA on a cloud HSM (a few hundred dollars a year) does the same job.
- **Timestamped**, so signatures stay valid after the certificate rolls over.
- **SmartScreen** may still warn on a newly signed app until it builds reputation (section 7.1).
- **[Proposal]** Changed 2026-10-26: now that the desktop app has replaced the Node bundle's own executable and `systray2`'s tray helper on Windows (section 1.1), what needs signing is shorter than this section once listed it: the NSIS installer itself, the app's one executable (`Retaskable.exe` or `RetaskableStaging.exe`, Electron's), and the keyring's `.node` — never a separate `retaskable.exe` or tray helper, which Windows no longer has. 5.4.2 says how.

### 5.4 The Windows download in detail (windows-release)

Sections 5.1 to 5.3 say what the Windows build is; this says how windows-release finishes it, end to end, for whoever builds it. Every line here is a **[Proposal]** (2026-10-26) unless it repeats a line above or one of section 2's **[Decided]** lines, and is the human's call. Nothing here changes a **[Decided]** line.

#### 5.4.1 Who builds what

- **[Proposal]** desktop-windows already builds and runs `RetaskableStagingSetup.exe` unsigned (5.1, 5.2): the window, the tray icon, the login item and the installer are built. windows-release adds signing, publishing and Production on top — the same shape as mac-release, but shorter, since Windows has no notarization step and no two-architecture choice (win-x64 only; arm64 later, [desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 4).
- **[Proposal]** Three slices, each one pull request:
  1. **Signed on staging**: signing and its CI secrets (5.4.2), so staging's download runs with no SmartScreen warning once it's built reputation (section 7.1) and updates itself.
  2. **Verified at the update** (5.4.5): the running app refuses a download signed by anyone else.
  3. **Production**: the signed installer published from retaskable.com, and Getting started's download (5.4.7).

#### 5.4.2 Signing (`scripts/winSign.ts`, new)

- **[Proposal]** **Where**: the same Linux CI machine that already builds every target (`node:22-bookworm`, installers.md section 3.10), with **jsign** (Java, Apache-2.0; pinned, its jar's SHA-256 checked, cached as Node's downloads are) calling Trusted Signing's Azure AD app over plain HTTPS for each file. Unlike notarizing a Mac app, which still needs a real Mac ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.7), nothing about an Authenticode signature needs Windows: jsign patches the PE file itself and asks Azure for the certificate, so no Windows machine and no Wine. This is what mac-release's signing section means by "windows-release's jsign".
- **The identity**: the Trusted Signing account's certificate profile, under the organisation's verified Trusted Signing account (section 5.3); its Subject name is what the running app compares a download against at the next update (5.4.5), and stays the same across every short-lived certificate Trusted Signing issues.
- **Inside out**, each with an RFC 3161 timestamp:
  1. The keyring's `.node`, the one native piece left in the app (section 1.1's table: the tray helper is Electron's own `Tray` now, not a second executable).
  2. The app's own executable (`Retaskable.exe` or `RetaskableStaging.exe`), once electron-builder has built it.
  3. The installer itself (`RetaskableSetup.exe` or `RetaskableStagingSetup.exe`), after it's packaged (5.4.3), so the installer's own signature covers the files it carries.
- **Checked before it's published**: `osslsigncode verify` (Linux-native, unlike `signtool`) on each signed file says it carries a valid, timestamped signature chaining to a trusted root; any one that doesn't fails the step and nothing is uploaded.
- **[Proposal]** **Secrets**: the Azure AD app's tenant id, client id and client secret, and the Trusted Signing account's endpoint and certificate profile name, as Woodpecker secrets for push events on `main` and production's branch only, never pull requests, so a pull request's code can't sign. Never in the repo (CLAUDE.md).
- **[Proposal]** **Pull requests** still build the installer unsigned, as today, so a broken bundle fails before merge.
- **Renewal**: nothing to renew — Trusted Signing issues a fresh short-lived certificate for each signing request under the same account, so there's no certificate expiry to plan around, unlike a Developer ID certificate.

#### 5.4.3 Packaged (electron-builder's NSIS)

- **[Proposal]** `npm run desktop -- --target win-x64 --environment staging|production [--publish]` (extending `scripts/desktop.ts`, [desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.10, to a second target) builds the app, signs it (5.4.2), then packages it with electron-builder's NSIS (`oneClick: false`, `perMachine: false`, `allowToChangeInstallationDirectory: false`): about 90 MB, roughly half the Mac universal build's 200 MB (desktop-app.md section 3.10), since it carries one architecture's Node and no second chip's keyring `.node`.
- **A copy already running**: electron-builder's NSIS installer checks for a running instance (the same single-instance lock the main process takes, desktop-app.md section 3.10) and closes it before replacing its files, so **Install** over an existing copy, or a silent update (5.4.5), never fails with a file in use.
- **[Proposal]** **Pull requests** build `--target win-x64` unsigned already (desktop-windows); this just adds the sign step in between, so the published package is never different from a pull request's but for its signature.

#### 5.4.4 The first run

What the person sees, from the download to the window:

- **[Proposal]** `RetaskableSetup.exe` downloaded and opened: **SmartScreen** may show "Windows protected your PC" until the signature's built reputation (section 7.1); Getting started's words (5.4.7) say **More info › Run anyway** is expected for now. Once open, its one page, "Start Retaskable when I sign in" (ticked) and **Install** (5.1), runs with no admin prompt, and opens the app when it finishes.
- **The window**: the desktop app's own ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.3), the environment's site at `/`, signed out or signed in, so the first thing a Windows user does after **Install** is the same **Set up Retaskable** flow a Mac or checkout user gets.
- **[Proposal]** **Antivirus**: a Node executable inside an installer is sometimes flagged (section 7.2); each release's installer is sent to Microsoft's false-positive submission before it's published, same as today's plan, until that stops being needed.

#### 5.4.5 The update

- **electron-updater's NSIS update** ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) sections 3.6, 3.12) checks staging's or production's `latest.yml` at `/api/agent/desktop/` and installs a newer signed build at the agent's restart, silently, with no admin prompt — the same mechanism already built for the Mac app's `.dmg`/`.zip` feed, pointed at Windows's files instead.
- **[Proposal]** **The same signature**: electron-updater's own check compares the download's Authenticode Subject name with the running app's; a download signed by anyone else, or not signed at all, is refused, the running copy keeps working, and the next hour tries again — the same shape as the Mac app's check (desktop-app.md section 3.6: its SHA-512, and there, the same Developer ID), just keyed on the certificate's Subject name instead of a Team ID.
- **[Proposal]** **The first signed build over an unsigned one** (today's testers): an unsigned running app has nothing to compare with, so the feed's hash is its only check; every update after that is compared.

#### 5.4.6 Uninstalling

- **Settings › Apps › Retaskable** (or **Retaskable Staging**) **› Uninstall** runs the NSIS uninstaller (built in, no admin prompt for a per-user install): it stops the agent first (as Quit does, asking first while sessions run, through the health check's port, so no file is still open when the uninstaller deletes it), then removes the Start Menu entry, the Run key value, the program folder and the data folder, and **[Proposal]** this environment's Windows Credential Manager entries only (`@napi-rs/keyring`'s, section 8.1) — never the other environment's, nor the shared backlog's link, which a checkout or the other environment may still use.
- **[Proposal]** Project checkouts and lane worktrees are never touched, as every uninstall in this spec promises.
- **[Proposal]** Uninstalling Production leaves Staging installed and running, and the other way round (section 8.1).

#### 5.4.7 Getting the download

- **[Proposal]** `/download/windows` ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 4's bullet, built the same way as section 3.8's `/download/mac`): the heading "Retaskable for Windows" ("Retaskable Staging for Windows" on staging, with its STAGING strip), one line ("Your projects, worked through one pull request at a time, from your PC. Needs Windows 10 (1809) or later."), **Download for Windows** (the `.exe` `latest.yml` names) with its version under it, and the steps: "1. Open the download.", "2. If Windows asks, choose More info, then Run anyway.", "3. Tick “Start Retaskable when I sign in” and choose Install.", "4. Sign in on the page that opens." With no build published yet it says "The Windows app isn't out yet." and no button, as `/download/mac` does with none of its own.
- **[Proposal]** `/start`'s first step, on a Windows browser, shows the same **Retaskable for Windows** button pointed at `/download/windows`, next to (not instead of) the Mac and Linux rows, the way `LinkActions` already picks one OS's download by the browser's own (desktop-app.md section 3.13's `isMacBrowser`, an `isWindowsBrowser` beside it).
- **[Proposal]** Today's text (Node, clone, `npm start`) moves under **Developing Retaskable?**, a closed section below, as mac-release proposes; whichever ships first moves it.

#### 5.4.8 Tests (replaces section 9's windows-release line)

- `scripts/winSign.test.ts`: the signing order (the keyring `.node`, then the executable, then the installer), each call's identity and timestamp, `osslsigncode verify` run on each, and a refused signature failing the step with nothing uploaded (jsign faked).
- `scripts/desktop.test.ts`: the NSIS options (`oneClick: false`, `perMachine: false`), the installer's size kept near 90 MB, and a pull request building `--target win-x64` unsigned.
- `desktop/src/updates.test.ts` (extended for Windows): a download whose Authenticode Subject differs from the running app's refused, an unsigned running app's taking the feed's hash alone, and a tried version not tried again.
- `agent/src/tray.test.ts`: Windows' **Start with Windows** tick reading and setting the Run key under Electron's login item, unchanged by this item.
- `app/src/startText.test.ts`: a Windows browser gets **Download for Windows**, pointed at `/download/windows`; with no feed entry, today's text.
- By hand, on a clean Windows 11 user with no admin rights: `RetaskableSetup.exe` downloaded, SmartScreen's warning (if any) passed with **Run anyway**, installed per user with the Start Menu entry and no admin prompt; the first signed build installing over an unsigned one with no extra prompt; a later signed version updating itself within the hour; a download signed differently refused; **Uninstall** from Settings › Apps removing the app, its Run key value and only this environment's Credential Manager entries; Production and Staging side by side.

#### 5.4.9 Acceptance

- Downloading `RetaskableSetup.exe` from `/download/windows` (or `/start`) on Windows and running it shows at most one SmartScreen question, installs with no admin prompt, and opens the window signed out (or at the last-opened project, signed in).
- `osslsigncode verify` accepts the installed app's executable and the installer that put it there, both carrying the same Trusted Signing identity.
- A newer version published to the feed installs itself within the hour, with no extra question, and the agent runs again on the new version with **Start with Windows** as the person left it.
- A download signed by a different identity is refused, and the running version stays.
- **Uninstall** from Settings › Apps removes the app, its Run key value, its data and only its own Credential Manager entries, and leaves every project checkout, the shared backlog's link and the other environment's app.
