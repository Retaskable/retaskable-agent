## 1. What it is

Today the agent is a checkout of this repo. You install Node, clone it, run `npm ci`, then `npm start`, and a terminal has to stay open for as long as it runs: closing it stops the agent. It updates itself by moving its own checkout forward to `origin/main` (agent.md section 4).

That is fine for the people building Retaskable and impossible for anyone else. Someone who has never opened a terminal should be able to install the agent, see its icon in the tray or menu bar, and never see a terminal or hear the word Node. On every OS.

So this spec makes a Production and a Staging build for each OS, all from one bundle:

| | Windows | macOS | Linux |
|---|---|---|---|
| **Production** ships as | A signed `RetaskableSetup.exe` from retaskable.com | A signed, notarized `.dmg` from retaskable.com | An AppImage from retaskable.com |
| Production updates | Its own (section 3.6) | Its own (section 3.6) | Its own (section 3.6) |
| **Staging** ships as | A signed `RetaskableStagingSetup.exe` | An `.app` (zip) | An AppImage |
| Staging updates | Its own | Its own | Its own |
| Starts at login | `HKCU\…\Run` | A LaunchAgent | XDG autostart |
| Its icon | Tray | Menu bar, no Dock icon | Tray (AppIndicator) |
| Terminal window | Never | Never | Never |

The checkout-and-`npm start` way stays for developing the agent itself (section 8).

### 1.1 The desktop app replaces it on macOS and Windows (2026-10-20)

**[Decided]** (2026-10-20) On macOS first, then Windows, what a person installs is the Electron desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md)): one app that runs the agent, the same code, in Electron's Node, with a window showing the web app on its environment. Where Electron covers them it replaces, on those two OSes:

| This spec's | Replaced by |
|---|---|
| The Node single executable and its folder (sections 3.1, 3.9) | Electron's Node running the bundled agent in a `utilityProcess` ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.1) |
| The tray helper (`systray2`) | Electron's `Tray`, drawn from the same menu model ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.4) |
| The zipped `.app` (section 3.15) and the planned `.dmg` (section 6.1) | electron-builder's signed, notarized `.dmg` and `.zip` |
| The LaunchAgent and Run key (section 3.14) | Electron's login item ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 3.5) |
| The release feed, download and install (sections 3.11, 3.12) | electron-updater from the environment's `/api/agent/desktop/` ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) sections 3.6, 3.7) |
| Inno Setup (sections 5.1, 5.2) | electron-builder's per-user NSIS installer ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 4) |

What stays the same: `build.json` and a release build's one environment, its folders (section 3.3), its log and crash dialog, the restart and `reason: "release"`, the tools it drives (section 3.4), signing with our Developer ID and Trusted Signing (sections 2, 5.3), and the agent's own code. Linux keeps sections 3 and 6.2 as they are, with `GET /api/agent/release` its feed. The table above, the decisions and the sections below describe what is built or planned for Linux, and for macOS and Windows until desktop-mac and desktop-windows are.

