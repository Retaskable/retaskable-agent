## 2. Decisions

- **[Decided]** (2026-10-18) Windows's Production is a signed installer downloaded from retaskable.com, not the Microsoft Store (the human: "I don't really want to rely on windows store"). We sign it ourselves (section 5.3).
- **[Decided]** Staging is a loose download, not in any store.
- **[Decided]** No build ever shows a console or terminal window, Staging included.
- **[Decided]** macOS and Linux work the same way, or as near as each OS allows (section 6).
- **[Decided]** Every agent is on the shared backlog automatically. The tray's **Shared backlog** submenu goes.
- **[Decided]** One approval click for the shared backlog, as part of installing, is fine (section 4.1).
- **[Proposal]** Every build comes from one bundle (section 3); only the OS and the environment baked in at build time differ. A release build links to **the one environment it was built for** (tray.md decision 7, changed to say so).
- **[Proposal]** No store on any OS, so every build updates itself the same way (section 3.6) and a fix reaches everyone within the hour, with no certification to wait for.
- **[Proposal]** No Mac App Store, no Flathub or Snap Store either (section 6.3): their sandboxes stop the agent from running the user's own `git`, `gh` and `claude` on the user's own folders, which is all it does.
- **[Decided]** (2026-10-18) Non-technical users are on Windows and macOS (the human: "Dumb users will be macos and windows"). Linux is for technical users: it gets the same agent with no terminal window, but its fixes can be a command to copy and its rough edges (section 6.2) are acceptable.
- **[Decided]** (2026-10-18) macOS uses the **Apple Developer Program, $99 a year**: without signing and notarization, macOS refuses to open the app and the only way past is System Settings › Privacy & Security › Open Anyway, which a non-technical user won't find. With Windows's signing (section 5.3), the two costs this spec can't avoid.

