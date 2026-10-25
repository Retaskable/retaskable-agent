## 8. Later

- Installers (`.msi`, `.dmg`, `.deb` / AppImage), code signing, start at login and auto-update. An installer makes a **release build**, locked to the environment it was built for (decision 7): the packaging marks it as one, and the agent reads that mark at start. Proposed in [installers.md](../installers.md) for Windows, macOS and Linux: Production as a signed download from retaskable.com, Staging from staging, each updating itself, none with a terminal window. On macOS and Windows that download is the Electron desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md), 2026-10-20).
- **Open on retaskable.com** at the item a notification was about.
