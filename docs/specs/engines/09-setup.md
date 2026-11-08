## 9. Setup

Built. The computer's settings page ([agent-settings.md](../agent-settings.md)) has the **Engines** choice: **Default engine** (Claude Code / Cursor) and one row per group of section 3.1 (Planner, Implementer, Review, Spec drafts, Chat, Release notes), each **Default**, **Claude Code** or **Cursor**. Beside it the page says "A session already running stays on the engine it started with. Cursor uses an allowlist, skips Claude Code's sandbox, and doesn't report a dollar cost. Dollar caps count Claude Code only." Once any role is on Cursor it also shows which allowlist file is in force (section 6.4). The daily-cap field says "Claude Code sessions only" once any role is on Cursor, and that cap doesn't stop a Cursor session (section 7).

- Cursor's choices are greyed out, with the reason, when `cursor-agent` isn't found or isn't signed in (section 4). The scan at the top of Setup also shows each engine's version or why it failed.
- "Shared by all projects" has the three Cursor model boxes (top, implementer, scout; `auto` by default), section 3.4.
- Saving applies in place, like any settings change (agent.md section 7): running sessions finish on their engine and the next ones use the new choice.
- **From the site and the app.** Settings → Project settings → **Engines** (the Expo app) sends the choice as the `engines` field of a `settings_request` ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1), whole, with the project's `engines.allow` kept. It changes which program the agent runs, so it is a command field: the agent checks the allow entries as the settings page does, and the computer's owner approves it on that computer before it runs. On a shared project the choice is the team's and is saved in the store ([project-settings-on-server.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-settings-on-server.md) section 3.3). The agent only checks, asks and applies.

### 9.1 A probe for each engine, and why one failed

**[Proposal]** Built for Claude Code and Cursor (`src/setup_check/mod.rs`); for pi and Codex today the scan only runs `--version` and shows nothing at all when the binary didn't answer (first-run-check.md section 3.1's optional `pi` and `codex`). This is the backlog item `engine-setup-probes`, in two slices.

1. **A probe for pi and Codex, like Cursor's.** `pi_check` and `codex_check` take the `--version` answer and say one of:
   - not installed (the binary wasn't found or never ran): a failed check, "pi isn't installed" or "Codex isn't installed", with the fix "Install pi from pi.dev." or "Install Codex: `npm install -g @openai/codex`, then run `codex login`.";
   - installed and signed in: a pass, the first line of `--version` ("codex-cli 0.46.0");
   - installed but `--version` exited non-zero: a failed check whose detail is the first line of stderr, cut to 200 characters like `first_line`, so the reason the engine failed shows instead of nothing;
   - Codex only: `codex login status` after a good `--version`; a non-zero exit is "Codex isn't signed in" with the fix "Run `codex login`.". pi has no sign-in check: its providers are whatever the person signed in to in pi itself (section 13), so a pi that answers `--version` passes.

   Neither feeds `setup_checks_pass` or `engine_ready`: a computer that has never installed pi or Codex still passes, as today. The probes run beside the others in `run_setup_checks`, never through a shell, under the same 20-second budget.
2. **A line for each engine, version or why it failed.** `setup_check_lines` shows a line for pi or Codex only when its binary was found on the PATH or named by `RETASKABLE_PI` / `RETASKABLE_CODEX`, so a computer without either keeps the report it has now. The line reads "✓ Codex: codex-cli 0.46.0" or "✗ Codex isn't signed in: run `codex login`." in `setup_check_line`'s existing format, and Claude Code's and Cursor's lines keep the same shape (version, or the reason). The settings page's Setup scan (agent-settings.md section 5) shows the same lines.

Tested in `src/setup_check/tests.rs`: each of the four states for both engines; a non-zero `--version` shows its stderr line; a signed-out Codex fails without changing `setup_checks_pass`; a computer with neither binary has no pi or Codex line.

Acceptance: with pi installed and Codex installed but signed out, the terminal's "Setup check:" shows "✓ pi …" and "✗ Codex isn't signed in: run `codex login`." and the check still exits 0 when git, `gh` and an engine are fine.

