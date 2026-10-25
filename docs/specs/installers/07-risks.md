## 7. Risks

### 7.1 SmartScreen on a new signature

A signed download can still get SmartScreen's "Windows protected your PC" until enough people have run it without trouble. Trusted Signing's certificates build reputation on the verified identity rather than on each short-lived certificate, which helps, but the first weeks may still warn. Getting started shows what the warning looks like and says **More info › Run anyway** is expected for now; Staging's testers build reputation before Production reaches anyone.

### 7.2 Antivirus on Node inside an exe

A Node single executable is sometimes flagged by antivirus heuristics. Signing every file (section 5.3) is most of the cure; at first, each release's installer is also sent to Microsoft's false-positive submission before it's published, until that stops being needed.

### 7.3 The tray helper inside packages

`systray2`'s helper is a second executable inside the Windows install, the `.app` and the AppImage. Each package's first build checks it starts and draws, and that the notification (`agent/src/notify.ts`: PowerShell's toast, `osascript`, `notify-send`) shows from inside it. On macOS the helper must be signed with the app and allowed by the hardened runtime.

### 7.4 Notarizing Node

A Node SEA needs the JIT entitlements (`com.apple.security.cs.allow-jit`, `allow-unsigned-executable-memory`) to run under the hardened runtime, and its signature must be applied after the agent's code is injected. mac-release's first build proves notarization passes before anything else.

