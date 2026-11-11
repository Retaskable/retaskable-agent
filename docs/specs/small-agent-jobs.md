# Small agent jobs: usage recheck, keep awake, native app facts, bug-report cuts — Spec

Status: v0.1, draft (2026-11-11). Backlog: `small-agent-jobs`. Copies the TypeScript agent's `usageCheck.ts` (limit-hand-off.md section 3.12), `keepAwake.ts` (tray.md section 4.2), `nativeApp.ts` (release-screen.md section 3.2) and #3358 (https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs). Builds on [agent/07.1-model-tiers.md](agent/07.1-model-tiers.md), [tray.md](tray.md) and [agent/07.3-retaskables-own-failures.md](agent/07.3-retaskables-own-failures.md).

Theme: Orchestration

Kind: new

## 1. What it is

Four small jobs the TypeScript agent does and the Rust agent does not: reread Claude's usage while the computer waits for the limit; hold the computer awake when asked; report a native app's facts; and cut a bug report's text without splitting an emoji.

## 2. Decisions

- **[Decided]** **Usage recheck.** While Claude's limit holds the computer (the tier says wait), every 10 minutes from the first tick, one tiny Haiku question (`claude -p "Reply with ok."`, one turn, no session kept, 90-second timeout) is read only for its rate-limit event, recorded as a session's would be, so a hold that no longer applies ends on the next tick. A reply with no event is an account with room; a refusal with no event, or a run that didn't finish, is no reading and the old one stands. No call when Claude is signed out or nothing waits. It never throws; the telemetry event is `usage_rechecked`.
- **[Decided]** **Keep awake.** While on, the OS does not sleep when idle, with no privileges: macOS `caffeinate -i -s -w <pid>`, Windows a PowerShell that sets the thread's execution state (system required) and waits for the agent, Linux `systemd-inhibit --what=idle:sleep` around `tail --pid`. The child ends with the agent. The choice is a file in the data folder, so a restart brings it back; stopping the agent lets the block go without forgetting the choice. A tool that is missing or ends by itself leaves it off.
- **[Decided]** **Native app facts.** For a project with `app/eas.json` on the base branch (else none): whether it builds for Android and iOS (the Expo config in `app/app.json`, else `app/app.config.js`, read as text and never run, or an `eas.json` build profile naming it), its version, and what changed since the last store build (the newest `submitted` line's sha in `testflight.log`): up to 100 changed paths and 50 commits touching `app/`, oldest first, each matched to an item by merge commit, an `Item:` trailer or the id its title starts with; the rest counted. Read-only, plain code and `git`; kept per base head and log stamp.
- **[Decided]** **No half emoji.** A bug report's signature, title and details are cut by characters of whole emoji: a cut never leaves half a surrogate pair (in Rust, never inside a character).
- **[Proposal]** The native-app read is only useful once a TestFlight lane exists ([testflight-restart-wait.md](testflight-restart-wait.md)); it is written first as a pure read the lane calls.
- **[Proposal]** The Rust names: `src/usage_check.rs`, `src/keep_awake.rs`, `src/native_app.rs`, and `text::truncate` (the first `max` characters, never inside one), which `src/triage/bug_report.rs` already uses for the signature, title and details.

## 3. Behavior, in build order

1. **The cut** is `text::truncate`, used by the bug report.
2. **Usage recheck**, called from the orchestrator tick.
3. **Keep awake** (`KeepAwake` in `src/keep_awake.rs`: `tick` starts or ends the child to match the choice file `keep-awake`, `set` records the choice), called at start-up and from the tray's refresh; the tray's menu item is *Keep this computer awake*, shown when the OS has a command; the command per OS is a pure function (`awake_command(os, pid)`) tested for each.
4. **Native app facts**, a pure read over a repo and a ref.
5. **Tests:** a usage reading from streamed lines (event, none but answered, none and refused); the 10-minute gap; keep awake's commands and its saved choice; the native-app read for an Expo app with each platform, none, more than 100 paths and 50 commits; the cut (`text::truncate` with an emoji at the limit). Ends with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## 4. Acceptance

- A computer waiting on a weekly limit that has lifted starts again within about 10 minutes.
- Turning Keep this computer awake on holds the OS awake on each of Windows, macOS and Linux, and it is still on after a restart.
- A native app's Android, iOS, version and changes since its last store build are read without running project code.
- No bug report ends in half an emoji.
