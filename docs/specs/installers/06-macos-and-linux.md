## 6. macOS and Linux

### 6.1 macOS (mac-release, staging-builds)

Since 2026-10-20 both builds are the desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3), built, signed and notarized by electron-builder, Staging's first, from `staging.retaskable.com/download/mac`. Packaging, signing, notarization, the `.dmg`, moving itself to Applications, start at login and uninstalling are all the desktop app's now, not this agent's — section 6.4 below is the plan that preceded that decision, superseded before any of it was built, kept only as a record of why the Mac app is shaped the way it is.

- **The menu-bar icon** and its menu are the desktop app's too (desktop-app.md section 3.4), drawn by Electron's main process from the menu this agent still hands it (`desktopShell.ts`'s `view`), not a native tray the agent owns on this OS.
- **Keychain**: `@napi-rs/keyring` uses the login keychain, as a checkout does today, whether this copy runs under the desktop app or (still possible, not yet removed) on its own.

### 6.2 Linux (linux-release, staging-builds)

- **Production**: an AppImage, `Retaskable-x86_64.AppImage`, from retaskable.com (built, section 3.18). The user marks it executable and opens it (Getting started says how, in one line, for their file manager). On first run it adds a desktop entry (`~/.local/share/applications/retaskable.desktop`, `Terminal=false`) so it shows in the app menu. No root.
- **Staging**: `Retaskable-Staging-x86_64.AppImage` from staging, the same way (built, section 3.17).
- **Start at login**: XDG autostart, `~/.config/autostart/retaskable.desktop`, with the menu's **Start at login** tick.
- **The tray icon** needs AppIndicator support (`libayatana-appindicator`, tray.md section 4.5). GNOME needs its AppIndicator extension; without it the agent runs with no icon (tray.md section 4.6's "Without a tray") and Getting started says what to install.
- **Credentials** go to the Secret Service (GNOME Keyring, KWallet) through `@napi-rs/keyring`. With none, the agent uses docker.md's credentials file in its data folder, readable by the user only.
- **Older distros** need `libfuse2` to run an AppImage; Getting started says so when the first-run check can't tell.
- **Headless Linux** (a server, ssh) stays as it is: the checkout or docker.md's container, with the terminal menu. [Proposal] Later, a `systemd --user` unit for the AppImage.
- **`.deb` and `.rpm`** are later, if people ask: they'd need root and a package repository for updates.

### 6.3 Why no store

- **Microsoft Store**: decided against (section 2). It would sign for free but hold every fix for certification, and MSIX redirects some of the files the agent and the tools it starts write under `%APPDATA%`.

- **Mac App Store**: every app must run in Apple's App Sandbox, which can only open folders the user picks in a dialog and can't run the user's own `git`, `gh` or `claude`.
- **Flathub**: Flatpak's sandbox needs `flatpak-spawn --host` to reach the host's tools and broad filesystem access, which Flathub's review pushes back on.
- **Snap Store**: the agent would need classic confinement, which needs Canonical's manual approval.

Each would turn "run the user's tools on the user's folders" into a fight with the sandbox. If one becomes worth it (people looking for it there), it's a later item.

### 6.4 The macOS download in detail (superseded, 2026-10-20)

Everything from here to the end of this section was mac-release's plan for building, signing and shipping the Mac app itself, before the 2026-10-20 decision moved that whole job to the desktop app (desktop-app.md section 3, built with electron-builder/electron-updater). None of the custom signing script, entitlements file, `.dmg` packaging, first-run move-to-Applications dialog or **Uninstall…** menu item described below was ever built, and none of it will be: section 6.1 above describes what actually ships. Kept as a record of the plan it replaced, not as still-open work.

#### 6.4.1 Who builds what

- **[Proposal]** staging-builds publishes the macOS zips unsigned (section 3.15 and its publishing step), and an unsigned arm64 executable doesn't start at all on a Mac, so today's Staging download can't be opened. mac-release signs **both** environments' apps, Staging's first: Staging's testers prove signing, notarization and the self-update on real Macs before Production's reaches anyone. If the human would rather keep Staging's signing in staging-builds (its "Still to come" names it), slice 1 below moves there unchanged.
- **[Proposal]** Three slices, each one pull request:
  1. **Signed and notarized on staging**: signing, the entitlements, notarization and stapling in CI (6.4.3 to 6.4.5), so the zip staging already publishes opens with no warning and updates itself.
  2. **The first run on a Mac**: moving itself to Applications (6.4.6), the keychain after the first signed build (6.4.8) and **Uninstall…** (6.4.9).
  3. **Production**: the `.dmg` (6.4.5) and production's feed entries.

#### 6.4.2 Two apps, one per chip

- **[Proposal]** **One app per architecture**, as built today (`mac-arm64` and `mac-x64`, section 3.15), not a universal one. A universal `.app` would need both Node executables joined with `lipo` and signed after the agent's code is injected into each, doubling every Mac's download (about 40 MB to 80 MB); the self-update already asks for its own `arch`, so each Mac only ever downloads its own.
- **[Proposal]** **Supported**: macOS 11 (Big Sur) and later, as `Info.plist`'s `LSMinimumSystemVersion` says now. Older macOS shows its own "You can't use this version of the application with this version of macOS", and nothing runs.
- **[Proposal]** **The wrong chip**: an Intel app on Apple silicon runs under Rosetta 2 (macOS offers to install it once). It works, slower, and its self-update keeps asking for `mac-x64`, since `build.json`'s `os` is what it asks for. An Apple silicon app on an Intel Mac doesn't open ("You can't open the application because it is not supported on this type of Mac"). Getting started's choice (6.4.10) is what keeps people off both.

#### 6.4.3 Signing (`scripts/macSign.ts`, new)

- **[Proposal]** **Where**: on the Linux CI machine, with **rcodesign** (apple-codesign, Rust, MPL-2.0; pinned, its SHA-256 checked, cached as Node's downloads are), which signs Mach-O files and bundles, notarizes through Apple's Notary API and staples, with no Mac. This mirrors windows-release's jsign. If it doesn't hold up when built (notarization refusing what it signed), a Mac signs instead, with Apple's `codesign`, `notarytool` and `stapler`, and this line changes.
- **The identity**: a **Developer ID Application** certificate from the Apple Developer Program, in the organisation's name; its Team ID is what section 3.12's update check compares (`macSignature`). The same certificate signs Staging's and Production's apps, as section 6.1 proposes.
- **Inside out**, each with the hardened runtime and Apple's timestamp:
  1. Each native piece in `Contents/Resources/node_modules/`: the keyring's `.node` and `systray2`'s tray helper executable.
  2. The agent's executable in `Contents/MacOS/`, with its entitlements (6.4.4). Node's own signature is removed before the agent's code is injected (`postject`) and the new one applied after, as section 7.4 says.
  3. The `.app` itself, its identifier `Info.plist`'s `CFBundleIdentifier` (`com.retaskable.agent`, or `com.retaskable.agent.staging`), which seals `Contents/Resources/`.
- **[Proposal]** **Native pieces in `Resources/`**: Apple wants code in `Contents/MacOS/`, `Frameworks/` or `Helpers/`; code in `Resources/` is accepted only when each file is signed by itself first (step 1). If notarization refuses it there, the tray helper moves to `Contents/Helpers/` and the `.node` to `Contents/Frameworks/`, and `bundleDir` (`release.ts`) learns those two folders. That's part of slice 1, not a new item.
- **Checked before it's published**: `rcodesign verify` on the `.app` says each Mach-O is signed by the Team ID, with the hardened runtime and a timestamp; any one that isn't fails the step and nothing is uploaded.
- **[Proposal]** **Secrets**: the Developer ID certificate and its key (a `.p12` and its password), and an App Store Connect API key for notarizing (issuer id, key id, the `.p8`), as Woodpecker secrets for push events on `main` and production's branch only, never pull requests, so a pull request's code can't sign. Never in the repo (CLAUDE.md).
- **[Proposal]** **Pull requests** still build both zips unsigned, as today, so a broken bundle fails before merge.
- **Renewal**: a Developer ID certificate lasts five years and is renewed under the same Team ID, so section 3.12's comparison, which reads the Team ID and not the certificate, still accepts the next update.

#### 6.4.4 The entitlements (`scripts/macos/entitlements.plist`, new)

- **[Proposal]** Only what Node needs under the hardened runtime (section 7.4):
  - `com.apple.security.cs.allow-jit`: V8's JIT.
  - `com.apple.security.cs.allow-unsigned-executable-memory`: V8's code pages.
  - `com.apple.security.cs.disable-library-validation` is **not** given: the `.node` is signed by the same Team ID, so library validation accepts it. If the keyring's prebuilt won't load without it when built, it's added and this line says why.
- **[Proposal]** No App Sandbox entitlement (section 6.3: the agent runs the person's own `git`, `gh` and `claude` on the person's own folders), and no camera, microphone or contacts entitlements: the agent never asks macOS for them, so macOS never asks the person. The tools the agent starts are their own signed apps and ask for their own permissions.
- **[Proposal]** **Folders macOS guards** (Desktop, Documents, Downloads, iCloud Drive): a project checked out there makes macOS ask once, "“Retaskable” would like to access files in your Documents folder.", the first time the agent or a tool it starts reads it. `Info.plist` gains `NSDocumentsFolderUsageDescription`, `NSDesktopFolderUsageDescription` and `NSDownloadsFolderUsageDescription`, each "Retaskable works on the projects you keep here.", so the question says why. A person who answers **Don't Allow** gets the first-run check's failure for that project (first-run-check.md: the folder can't be read), with "Allow Retaskable in System Settings › Privacy & Security › Files and Folders." as its fix.

#### 6.4.5 Notarized, stapled and packaged

- **Notarizing**: the signed `.app` is zipped and sent to Apple's Notary API (`rcodesign notary-submit --wait`); the step waits up to **30 minutes**, then fails. A refusal fails the step with Apple's log (which file, why) printed, and nothing is uploaded.
- **Stapling**: the ticket is stapled to the `.app` (`rcodesign staple`), so Gatekeeper accepts it with no network; then the published zip is made again from the stapled `.app` (section 3.15's zip, Unix modes kept), and that zip's SHA-256 is the one its feed entry names.
- **[Proposal]** **The `.dmg`** (Production's first download only, section 6.1): `Retaskable-mac-arm64.dmg` and `Retaskable-mac-x64.dmg`, each holding `Retaskable.app` and a link to `/Applications`, with a plain background saying "Drag Retaskable to Applications". Made on Linux with libdmg-hfsplus's `dmg` over an HFS+ image (`hfsplus` tools, pinned), then signed, notarized and stapled itself. If no `.dmg` tool holds up on Linux, Production's first download is the notarized zip (Safari unpacks it into Downloads by itself) and the move in 6.4.6 does the rest; this line then changes, and section 6.1's ".dmg" with it.
- **[Proposal]** **The feed stays the zip**, never the `.dmg`: section 3.12's update unpacks a zip with `ditto`, which needs no mounting and no window.
- **[Proposal]** **Staging's first download stays its zip** (section 6.1): its testers are the people building Retaskable.
- **Staging** publishes on every push to `main`, as staging-builds does now, once signed: the zip at `downloads/<version>/Retaskable-Staging-mac-arm64.zip` (and `-mac-x64`), then `mac-arm64.json` and `mac-x64.json`, in that order.
- **[Proposal]** **Production** publishes from the same pipeline on the push that deploys production ([environments.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/environments.md) section 4, Promote), building `--environment production` from that commit into `retaskable-agent-releases-production`: the two zips and their feed entries, and the two `.dmg`s beside them at `downloads/<version>/`, so Production's agent only ever gets a commit Staging's has already run (as windows-release proposes for Windows).

#### 6.4.6 The first run: in Applications

What the person sees, from the download to the menu-bar icon:

- **[Proposal]** **Production**: the `.dmg` opens a Finder window with the app and the Applications link; they drag it across and open it from Applications or Launchpad. Gatekeeper's first-open question, "“Retaskable” is an app downloaded from the Internet. Are you sure you want to open it?" with **Open**, is macOS's own and expected (a notarized app gets it once, with no warning). The icon appears in the menu bar, at the top right, and the browser opens the retaskable.com link page (section 4: link this computer, then the shared backlog).
- **[Proposal]** **Not in an Applications folder** (opened straight from the `.dmg` under `/Volumes/`, from Downloads, or from wherever the zip was unpacked): macOS runs a quarantined app that hasn't been moved from a random read-only copy (App Translocation, a path under `/private/var/folders/…/AppTranslocation/`), where start at login would name a path that's gone after the next restart and the self-update can't replace it (section 3.12). So a macOS release build checks where it runs **before** it links or starts anything, and when it isn't in `/Applications/` or `~/Applications/` shows, through `osascript`'s `display dialog`: "Retaskable needs to be in your Applications folder to start at login and keep itself up to date." with **Move to Applications** (default) and **Quit**.
  - **Move to Applications** copies the original `.app` (with `ditto`; the original's path, not the translocated one, is the one macOS reports for it, else the path it runs from) to `/Applications/` when this user can write there, else to `~/Applications/` (made when missing); removes the quarantine attribute from the copy (`xattr -dr com.apple.quarantine`: the app is notarized and Gatekeeper has already passed it); starts the copy; and quits. An app already there with the same name is moved to the Bin first, after a running one is asked to quit (through its health check's port). The `.dmg` stays mounted: ejecting is the person's.
  - **Quit** quits, and the next open asks again.
  - A failure (no space, a folder it can't write) shows "Couldn't move Retaskable: <reason>. Drag it to Applications yourself, then open it from there." and quits.
- **[Proposal]** **A standard (non-admin) user**: `/Applications/` isn't theirs to write, so the move goes to `~/Applications/`, and so does every later update (section 3.12 replaces the `.app` where it is). An app an admin put in `/Applications/` and a standard user runs can't replace itself: it's `installerFor`'s `cannot`, checked before anything downloads, and logs once "Update: 1.4.3 is out, but this copy can't replace itself (its folder, /Applications, isn't yours to write)."; the tray's status line adds "· update needs an admin".
- **The rest of the first run** is section 3.14's: the LaunchAgent written, on, on the first run only. macOS 13 and later shows its own "Background Items Added" notice; signed, it names Retaskable and its developer rather than the bare executable.
- **[Proposal]** **Turned off in System Settings › General › Login Items**: macOS 13's switch there stops the LaunchAgent loading without removing its file, so the menu's **Start at login** tick, which reads the file, still shows on. Clicking it off and on rewrites the file, which macOS doesn't turn back on by itself. On macOS 13 and later the tick's tooltip says "If it doesn't start at login, check System Settings › General › Login Items." That's accepted for now: reading the switch itself needs Apple's `SMAppService`, native code, and is a later step, not an item.

#### 6.4.7 The update on a Mac (section 3.12, with a signed app)

- Once signed, section 3.12's check applies: a download whose Team ID differs from the running app's, or that fails `codesign --verify --deep --strict`, is refused and its folder deleted; the next hour asks again.
- **[Proposal]** **The first signed build over an unsigned one** (Staging's testers today): the unsigned app has nothing to compare with, so the feed's SHA-256 is its only check (section 3.12), and it installs the signed one. From then on every update is compared.
- **[Proposal]** **No quarantine on an update**: the agent downloads with Node, which sets no quarantine attribute, so the new `.app` opens with no Gatekeeper question; the signature check above stands in for it.
- **[Proposal]** **The LaunchAgent** names the executable inside the `.app`, whose path the update keeps, so it isn't rewritten; the menu-bar icon goes and comes back as on any restart.

#### 6.4.8 The keychain

- **[Proposal]** Each link's secrets are generic passwords in the login keychain (`@napi-rs/keyring`, section 6.1). macOS lets the app that wrote one read it back without asking as long as it's the same app by its signature (its identifier and Team ID), so a signed app updated by a signed app of the same Team ID never asks.
- **[Proposal]** **Once, after the first signed build** over an unsigned one, macOS asks "Retaskable wants to use your confidential information stored in “…” in your keychain.", since the unsigned app that wrote it isn't the same app. **Always Allow** keeps the link. The release notes for the first signed Staging build say so in one line. **Deny** leaves the agent unable to read its link, which it shows as not linked (the tray's **Link to**), and linking again replaces the entry.
- **[Proposal]** **A locked or missing login keychain** (rare: a renamed account, a reset keychain): the agent uses docker.md's credentials file in its data folder, readable by the user only, as section 6.2 says for Linux with no Secret Service (`credentialEntry`, `credentials.ts`), and logs so once.

#### 6.4.9 Uninstalling (**Uninstall…**)

macOS has no uninstall hook, so the menu does it (section 6.1):

- **[Proposal]** **Uninstall…**, last before Quit, in a macOS release build only, asks through `osascript`: "Remove Retaskable? Your projects and their code stay where they are." with **Remove** and **Cancel** (default).
- **[Proposal]** **Remove** stops every lane's session as Quit does, then removes the LaunchAgent (its file, and `launchctl bootout` for this login), the data folder (`~/Library/Application Support/Retaskable/`), the logs (`~/Library/Logs/Retaskable/`) and only this environment's keychain entries (as windows-release's `--forget-link` does on Windows: the shared backlog's link stays, since the other environment or a checkout may use it, section 8.1); moves the `.app` to the Bin through Finder (`osascript`'s `tell application "Finder" to delete`, so **Put Back** works); and quits. Project checkouts and lane worktrees are never touched.
- **[Proposal]** **Dragging the app to the Bin** instead is allowed (section 6.1): what's left (the LaunchAgent's file, the data, the logs) does nothing, since at the next login launchd finds no executable and starts nothing. The website shows the computer offline, as for any computer that stops.
- **[Proposal]** Uninstalling Production leaves Staging installed and running, and the other way round.

#### 6.4.10 Getting the download

- The download page (`/start`'s first step, **Install the agent**) is the app's: it offers **Download for Mac (Apple silicon)** and **Intel Mac** from the environment's feed, each the `.dmg` beside the feed's zip for `mac-arm64` or `mac-x64`, since a browser can't tell which chip a Mac has. The agent's part is that each is a build that opens and updates itself as above.

#### 6.4.11 Tests (adds to section 9's mac-release line)

- `scripts/macSign.test.ts`: the signing order (native pieces, then the executable with its entitlements, then the `.app`), each call's identity, hardened runtime and timestamp; the entitlements file holds exactly 6.4.4's two keys; a refused notarization fails with Apple's log and uploads nothing (rcodesign faked).
- `scripts/package.test.ts`: the `.dmg`'s name per architecture, Production only; the zip made again after stapling, its SHA-256 the one printed; `Info.plist`'s three folder usage descriptions.
- `agent/src/release.test.ts` (or a new `macMove.test.ts`): where an app runs, read as in Applications (`/Applications/`, `~/Applications/`) or not (`/Volumes/…`, `~/Downloads/`, a translocated path); the move's target, `/Applications/` when writable and `~/Applications/` when not; the dialog's text and buttons (`osascript` faked); a failure's text.
- `agent/src/releaseInstall.test.ts`: a `.app` in a folder this user can't write is `cannot`, with its line, before anything downloads.
- `agent/src/tray.test.ts`: **Uninstall…** only in a macOS release build, last before Quit; **Remove** removes the LaunchAgent, data, logs and only this environment's keychain entries, moves the `.app` through Finder, and touches no project folder (each faked).
- By hand, on a clean macOS user on Apple silicon and on Intel, for each slice that changes the app: the `.dmg` opened and dragged across, with only Gatekeeper's one question; opened from the `.dmg` and from Downloads, and moved; a standard user's move to `~/Applications/`; an update installing by itself and the agent running again; the first signed build over an unsigned one, with its one keychain question; a project in Documents, with its one question; **Uninstall…**; Production and Staging side by side.

#### 6.4.12 Acceptance

- Downloading `Retaskable-mac-arm64.dmg` from `/start` on an Apple silicon Mac, dragging the app to Applications and opening it shows only macOS's one "downloaded from the Internet" question, then the menu-bar icon, and opens the link page.
- `spctl --assess --type execute` accepts the installed `.app` ("source=Notarized Developer ID") and `stapler validate` finds its ticket; Staging's zip, unpacked on a Mac, is accepted the same way.
- Opened from anywhere but an Applications folder, it offers **Move to Applications**, and after the move it runs from there and starts at login.
- A newer version published to the feed installs itself within the hour, with no question from macOS or the keychain, and the agent runs again on the new version with its **Start at login** tick as the person left it.
- A download signed by another Team ID is refused, and the running version stays.
- **Uninstall…** removes the app, its LaunchAgent, its data, its logs and its own link, and leaves every project checkout, the shared backlog's link and the other environment's app.

