# The Agent Installers — Spec

Status: v0.1, proposal (2026-10-18). It fills in [tray.md](tray.md) section 8's "Installers, code signing, start at login and auto-update" for Windows, macOS and Linux, and changes the tray's **Shared backlog** ([tray.md](tray.md) section 4.6) and the backlog store's machine link ([backlog.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/backlog.md) section 4.2). **[Decided]** lines are the human's, from 2026-10-18; **[Proposal]** lines fill the gaps and are the human's call. Changed 2026-10-20: on macOS, then Windows, the agent ships inside the Electron desktop app ([desktop-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/desktop-app.md)), whose window is the web app; it replaces this spec's Node bundle, zipped `.app`, Inno Setup installer and own update code there (section 1.1). Linux keeps them. See also [setup-in-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/setup-in-app.md). This copy leaves out what the hub serves (the release feed and downloads) and what the app shows (the download pages); see [TRIMMED.md](TRIMMED.md).

Theme: Website

Kind: new

This spec is split: each section below is its own file in [installers/](installers). Read this index, then only the sections you need.

## 1. What it is

- [installers/01-what-it-is.md](installers/01-what-it-is.md): 1.1 The desktop app replaces it on macOS and Windows (2026-10-20)

## 2. Decisions

- [installers/02-decisions.md](installers/02-decisions.md)

## 3. The bundle (agent-bundle)

- [installers/03-the-bundle-agent-bundle.md](installers/03-the-bundle-agent-bundle.md): 3.1 What's in it · 3.2 No terminal window · 3.3 Where its files go · 3.4 The tools it drives · 3.5 What a checkout had that a bundle hasn't · 3.6 Updating itself (agent-self-update) · 3.7 Details (agent-bundle) · 3.8 Details (agent-bundle) · 3.9 Details (agent-bundle) · 3.10 Details (agent-bundle) · 3.11 Details (agent-self-update) · 3.12 Details (agent-self-update) · 3.13 Details (staging-builds) · 3.14 Details (staging-builds) · 3.15 Details (staging-builds) · 3.16 Details (staging-builds) · 3.17 Details (staging-builds) · 3.18 Details (linux-release)

## 4. Every agent on the shared backlog (shared-backlog-auto-link)

- [installers/04-every-agent-on-the-shared-backlog-shared.md](installers/04-every-agent-on-the-shared-backlog-shared.md): 4.1 Why one approval is still needed

## 5. Windows

- [installers/05-windows.md](installers/05-windows.md): 5.1 Production: a signed installer (windows-release) · 5.2 Staging: the same installer (staging-builds) · 5.3 Signing

## 6. macOS and Linux

- [installers/06-macos-and-linux.md](installers/06-macos-and-linux.md): 6.1 macOS (mac-release, staging-builds) · 6.2 Linux (linux-release, staging-builds) · 6.3 Why no store · 6.4 The macOS download in detail (mac-release)

## 7. Risks

- [installers/07-risks.md](installers/07-risks.md): 7.1 SmartScreen on a new signature · 7.2 Antivirus on Node inside an exe · 7.3 The tray helper inside packages · 7.4 Notarizing Node

## 8. What stays

- [installers/08-what-stays.md](installers/08-what-stays.md): 8.1 Both builds on one computer

## 9. Tests

- [installers/09-tests.md](installers/09-tests.md)
