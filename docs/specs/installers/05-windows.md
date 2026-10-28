## 5. Windows

### 5.1 Production: the desktop app's installer (windows-release)

Since 2026-10-20 what a person installs on Windows is the Electron desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 4), built by electron-builder's NSIS, not an Inno Setup installer built from this repo. This agent's own part is the single executable the desktop app runs in a `utilityProcess` (installers.md section 3.1); the installer, the tray icon, start at login and the signing below are windows-release's, in the desktop app's own repo (`desktop/`, `scripts/desktop.ts`), not this one.

- **`RetaskableSetup.exe`**, from retaskable.com's Getting started, built by electron-builder's NSIS. It installs **per user** into `%LOCALAPPDATA%\Programs\Retaskable\`, so there's no admin prompt, adds a Start Menu entry, and opens the app when it finishes. Its only page is "Start Retaskable when I sign in" (ticked) and **Install**.
- **Start at login** through Electron's `setLoginItemSettings`, which sets the same `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key; the tray's menu gets **Start with Windows** (a tick) that sets the same.
- **Uninstall** from Settings › Apps, which stops the app and removes the program and data folders but not project checkouts.
- **[Proposal]** An `.msi` (WiX) too, later, only if a company asks for one to deploy with its own tools. The `.exe` is simpler to build and installs per user without admin.

### 5.2 Staging: the same installer (windows-release)

`RetaskableStagingSetup.exe`, from staging's Getting started: the same installer with the Staging bundle, into `%LOCALAPPDATA%\Programs\Retaskable Staging\`, with its own Run key value and uninstall entry. Today it ships unsigned; signing is section 5.3's, once windows-release builds it.

### 5.3 Signing

- **[Proposal]** **Azure Trusted Signing** (about $10 a month, Basic): no hardware token, signing from CI with a short-lived certificate, and the signature chains to Microsoft's trusted roots. Every executable is signed: the installer, the app's executable and the keyring's `.node`.
- **Eligibility** is checked first: Trusted Signing verifies the organisation (or, where offered, the individual) before it issues anything. If Retaskable can't qualify, an OV certificate from a commercial CA on a cloud HSM (a few hundred dollars a year) does the same job.
- **Timestamped**, so signatures stay valid after the certificate rolls over.
- **SmartScreen** may still warn on a newly signed app until it builds reputation (section 7.1).

