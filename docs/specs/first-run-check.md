# First-run check — Spec

Status: v0.1, draft (2026-10-11). From an audit of Retaskable on 2026-10-11 (a new user gets a three-step Getting started guide that checks nothing: a missing or signed-out `claude` only shows up as a failed session later, and installers are left for later in tray.md section 8). Section 2's **[Decided]** lines are what the human asked for; the **[Proposal]** lines fill the gaps and are the human's call. Changes Getting started ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 4.2, build order getting-started), the agent's `hello` ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 5.3) and its engine probe ([engines.md](engines.md)).

The TypeScript agent in Retaskable.com is the ground truth for behaviour (`agent/src/…` paths below). This Rust agent ports the same behaviour as its pieces land: section 3.1's checks (minus `node`, which only matters to the TypeScript runtime) live in `src/setup_check.rs`, run from `src/runtime.rs` at start and from a `check` subcommand — this item's own, standalone part. The rest of section 3.1 isn't a further slice of this item: the cloud connector (`src/connector/`) sends `hello` but not yet its `checks` field, and `SetupCheckRunner`'s 10-minute re-run and the read-only `setup_check` command land with command handling ([11-build-milestones.md](agent/11-build-milestones.md) milestone 5). The trial plan (3.3) is its own backlog item, `trial-plan-draft`, waiting on a drafts module this port hasn't built.

Theme: Website, Setup

Kind: new

## 1. What it is

Today a new user reads three steps on retaskable.com (run the agent, link it, approve the code) and then reloads the page. Nothing checks that Claude Code is installed, that it's signed in, that `gh` is signed in, or that a change can actually go all the way through. When something's missing they find out from a failed session, often much later.

This replaces Getting started with **one page of five steps**. Each step is checked automatically and turns green by itself. A step that fails says, in plain words, what to do.

## 2. Decisions

- **[Decided]** A new user is walked through: installing the agent; checking `claude` (or `cursor-agent`) is installed and signed in; linking the computer; linking a project; and a trial change. The human asked for this on 2026-10-11 after the audit. The agent's part is the checks (section 3.1) and the trial plan (section 3.3).
- **[Decided]** Each step is checked automatically, with a plain fix when it fails.
- **[Proposal]** The agent runs the checks itself, at start and on request, and reports them in its `hello`. retaskable.com never reaches into the machine: the agent's port stays `127.0.0.1` only (CLAUDE.md).
- **[Proposal]** Before the computer is linked, the site can't see the machine. So the agent also prints the same checks in its terminal at start (`npm start`) and in `npm run check`, and the page's step 2 says "Checked once this computer is linked" until then.
- **[Proposal]** **Claude Code or Cursor**: step 2 passes when either engine is ready. Claude Code is the default; Cursor's line shows only when Claude Code isn't ready or the project uses Cursor (engines.md).
- **[Proposal]** **The trial change** is a small plan: "Add a line to the README saying this project uses Retaskable." It never approves itself, so nothing merges until the person presses **Approve plan**. The step passes once the plan's pull request is open, which proves Claude ran, signed in, on their repo, and `gh` could push.
- The page that shows the steps (`/start`), when the home opens it and when the profile menu hides **Setup** are the app's, not the agent's. The agent only supplies the checks.
- **[Proposal]** One new read-only cloud command, `setup_check`, for **Check again**. It runs only the fixed probes and takes no arguments (built, in [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1's table).

## 3. Behavior

TypeScript paths are Retaskable.com's; Rust paths appear as this agent ports each piece.

### 3.1 The checks, on the agent (slice 1, built)

- `agent/src/setupCheck.ts`, `runSetupChecks(): Promise<SetupChecks>`, each check `{ ok: boolean | null, detail, fix? }` (`null`: couldn't tell). Runs with `run` (`agent/src/proc.ts`), 20 s each, never through a shell:
  - `node`: `process.versions.node` is 22 or later. Fix: "Install Node 22 or later from nodejs.org, then run `npm start` again."
  - `git`: `git --version`. Fix: "Install git from git-scm.com." While Retaskable's own copy of git is in use (a folder project on a computer with none, [code-storage.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/code-storage.md) section 3.2.1), it says "Retaskable's own copy: git version …".
  - `ghInstalled` (since install-tools, [setup-in-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/setup-in-app.md) section 5.4): `gh --version` answers, shown as its first line ("gh version 2.103.0 (…)"); a missing one says "The GitHub CLI isn't installed" with `ghMissing`'s fix. Optional in `SetupChecks`, so an older agent's checks still parse; the terminal's lines don't show it.
  - `gh`: `gh auth status` exits 0. Fix: "Run `gh auth login` in a terminal on this computer." A missing `gh` says "The GitHub CLI isn't installed", fix "Install the GitHub CLI from cli.github.com, then run `gh auth login`."
  - `claude`: `probeEngines` (`agent/src/engines/probe.ts`, which now takes the `run` to use) as today, shown as "Claude Code 2.1.4". Fix: "Install Claude Code: `npm install -g @anthropic-ai/claude-code`."
  - `claudeSignedIn`: `ANTHROPIC_API_KEY` set in the agent's environment passes without asking; otherwise `claude auth status`, which answers JSON with `loggedIn`. It fails only on an answer that says it's signed out (`loggedIn: false`, or "not logged in"); it passes on exit 0 with `loggedIn: true` (or no JSON and no "unknown"/"usage"). Anything else (an older Claude Code that doesn't know the command, a timeout) is `null`, "Couldn't tell whether Claude Code is signed in; the trial change will show it". `null` too, "Checked once Claude Code is installed", while `claude` is missing. The account's email and the rest of the JSON stay on the machine. Fix: "Run `claude` in a terminal on this computer and sign in."
  - `cursor`: the probe's Cursor answer (installed, then `cursor-agent status`). Fix: "Run `cursor-agent login`.", or when it isn't installed "Install Cursor's agent from cursor.com/cli, then run `cursor-agent login`."
  - `pi` and `codex` (optional, like `ghInstalled`): `pi --version` or `codex --version`, only when the binary answered, so an engine nobody installed adds nothing. They feed the computer's own Now tab (agent-settings.md section 4.5) and don't count toward `setupChecksPass`.
- `SetupChecks` and `SetupCheck` in `shared/types.ts`; the words in `shared/setupCheckText.ts`, so the agent's terminal and the page say the same: the fixes (`SETUP_FIXES`), `claudeReady` (installed and not known signed out), `engineReady` (Claude Code or Cursor), `showCursor` (Claude Code not ready), `setupChecksPass` (Node, git, a signed-in gh and an engine) and the lines (`setupCheckLines`, `setupCheckReport`): "✓ detail", "✗ detail: fix" (the fix's first letter lowered), "? detail" for `null`.
- **At start**, `agent/src/index.ts` runs them once the port is up and prints "Setup check:" and one line each ("✓ Claude Code 2.1.4", "✗ gh isn't signed in: run `gh auth login` …"), Cursor's only when Claude Code isn't ready. `npm run check` (`retaskable check`, `agent/src/cli.ts`) prints the same and exits 1 unless `setupChecksPass`.
- **In `hello`**: an optional `checks: SetupChecks` (`shared/cloud.ts`; older agents send none, and none goes before the first run). A missing git, GitHub CLI or Claude Code has an **Install** button on the page ([setup-in-app.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/setup-in-app.md) section 5), and the agent's start puts a GitHub CLI it fetched and `~/.local/bin` on its own PATH, so the checks find them. `SetupCheckRunner` (`agent/src/setupCheck.ts`) keeps the latest, runs one at a time, and runs again every 10 minutes (whether or not a site is linked: it's a few quick commands); a changed result marks every connection changed (`clouds.changed(null)`), so `hello` goes again within a second.
- **`setup_check`** (read-only, `{}`): runs them now and answers `SetupChecks` as its `ack`; a changed result goes up in `hello` too. It's taken before Setup (the checks are the machine's), refused on a view-only agent like every command, and any argument is refused.
- In this Rust agent: `src/setup_check.rs` has `SetupCheck` / `SetupChecks` and `run_setup_checks`, with the same checks minus `node` (a compiled binary has no Node runtime version to check). `src/runtime.rs` runs them once the port is up and prints the same "Setup check:" lines, and a `check` subcommand prints them and exits 1 unless they pass, matching `npm run check`. `src/connector/` sends `hello` without `checks`, since the hub's schema asks for a `node` check this agent doesn't have; `SetupCheckRunner`'s 10-minute re-run and the `setup_check` command aren't ported here either — they're carried by command handling instead ([11-build-milestones.md](agent/11-build-milestones.md) milestone 5), not by a further slice of this item. There's no drafts module yet, so the trial plan (3.3) isn't ported.

### 3.2 The page

The page of steps (`/start`, nine steps since setup-steps), its badges, its "Check again" and "I'm done here", and its home redirect are the Expo app's. It shows the agent's check lines from `hello.checks` as they come, and sends `setup_check` for **Check again** (the command [cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1 lists). The agent does not draw or serve any of it.

### 3.3 The trial plan

- A trial draft is an ordinary draft the agent writes, marked `trial: true` by the `draft_spec` command. The agent's `approvesItself` (`agent/src/drafts.ts`) answers false for it, so it always waits for **Approve plan** ([simple-flow.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/simple-flow.md) section 2). Allowing one trial at a time per project is the store's to decide.
- This Rust agent has no `drafts` module or `draft_spec` command yet, so the trial plan isn't ported here. It's its own backlog item, `trial-plan-draft`, not a slice of this item; it waits on a drafts module this port hasn't built.


## 4. Tests

- TypeScript (Retaskable.com): `agent/src/setupCheck.test.ts`, `agent/src/connector.test.ts`, `agent/src/commands.test.ts`, `agent/src/simpleFlow.test.ts`.
- `agent/src/setupCheck.test.ts` (built): each check with `run` stubbed: passing (never through a shell, the email left out), a missing binary (`ENOENT`), a non-zero exit, `claude auth status` unknown (`null`), `ANTHROPIC_API_KEY` set, Cursor enough without Claude Code; the terminal lines; the runner's one run at a time and change.
- `agent/src/connector.test.ts` (built): no `checks` before the first run, and `hello` again with them once they change.
- `agent/src/commands.test.ts` (built): `setup_check` answers the checks, before Setup too; a view-only agent refuses it.
- `agent/src/simpleFlow.test.ts` (built): a trial draft never approves itself; one open at a time, a second answering the first's id, a new one once it's closed.
- Rust (this agent): `src/setup_check.rs` (section 3.1's checks minus `node` and the terminal lines; `resolve_cursor` lives in `src/engines/resolve.rs`). `src/connector/` sends no `checks` in its hello, since the hub's schema asks for a `node` check this agent doesn't have. The commands and trial-plan tests have no module yet.

## 5. Acceptance

- On a fresh machine with Claude Code installed but not signed in, `npm start` prints "✗ Claude Code isn't signed in: run `claude` …", and after linking, `/start`'s step 2 says the same, then turns **Done** within a minute of signing in and pressing **Check again**.
- With `gh` signed out, step 1 shows the `gh auth login` fix.
- The trial plan doesn't merge until **Approve plan** is pressed.
- TypeScript: `npm run test:changed` and `npm run lint` pass. Rust: `cargo test setup_check` and `retaskable check` on this machine print the same lines and exit code `npm run check` would.
