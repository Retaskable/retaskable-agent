## 5. Windows

### 5.1 Production: a signed installer (windows-release)

Since 2026-10-20 the installer is electron-builder's NSIS, not Inno Setup, and installs the desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md) section 4); what it does is as below.


- **`RetaskableSetup.exe`**, from retaskable.com's Getting started, built with Inno Setup. It installs **per user** into `%LOCALAPPDATA%\Programs\Retaskable\`, so there's no admin prompt, adds a Start Menu entry, and starts the agent when it finishes. Its only page is "Start Retaskable when I sign in" (ticked) and **Install**.
- **Start at login** through the user's `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key; the tray's menu gets **Start with Windows** (a tick) that sets the same.
- **Uninstall** from Settings › Apps (an uninstall entry under `HKCU`), which stops the agent and removes the program and data folders but not project checkouts.
- **[Proposal]** An `.msi` (WiX) too, later, only if a company asks for one to deploy with its own tools. The `.exe` is simpler to build and installs per user without admin.

### 5.2 Staging: the same installer (staging-builds)

`RetaskableStagingSetup.exe`, from staging's Getting started: the same installer with the Staging bundle, into `%LOCALAPPDATA%\Programs\Retaskable Staging\`, with its own Run key value and uninstall entry. Signed with the same identity, since the signing is paid for either way and an unsigned exe with Node inside gets quarantined by Defender often enough to matter.

### 5.3 Signing

- **[Proposal]** **Azure Trusted Signing** (about $10 a month, Basic): no hardware token, signing from CI with a short-lived certificate, and the signature chains to Microsoft's trusted roots. Every executable is signed: the installer, `retaskable.exe`, the tray helper and the keyring's `.node`.
- **Eligibility** is checked first: Trusted Signing verifies the organisation (or, where offered, the individual) before it issues anything. If Retaskable can't qualify, an OV certificate from a commercial CA on a cloud HSM (a few hundred dollars a year) does the same job.
- **Timestamped**, so signatures stay valid after the certificate rolls over.
- **SmartScreen** may still warn on a newly signed app until it builds reputation (section 7.1).

