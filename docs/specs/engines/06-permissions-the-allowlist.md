## 6. Permissions: the allowlist

Built (`prepareCursor`), with the extra entries of section 6.4. The repo's rules say Claude sessions run with `--permission-mode auto` and never skip permissions. Cursor has no auto mode: headless, it either runs everything (`--force`) or only what its permissions allow. Retaskable takes the second.

### 6.1 Where it lives

- Cursor reads `.cursor/cli.json` in the workspace (and the user's own `~/.cursor/cli-config.json`): `{ "permissions": { "allow": [...], "deny": [...] } }`, with entries like `Shell(git)`, `Read(**)` and `Write(src/**)`.
- Before each Cursor session, `prepare(spec)` writes Retaskable's allowlist for that role to `<lane>/.cursor/cli.json` and adds `.cursor/cli.json` to the lane's `.git/info/exclude`, so the implementer can't commit it by accident. Retaskable writes it fresh every time, so a session that edits it gains nothing past its own run.
- If the repo tracks its own `.cursor/cli.json`, Retaskable doesn't overwrite it: it uses it, and refuses to start a Cursor session when it allows any shell command (`Shell(*)`), writes outside the workspace (`Write` of `/…`, `~…` or `../…`), or lacks the deny list below. The user's `~/.cursor/cli-config.json` is refused for `Shell(*)` and those outside writes, not for a missing deny list. The refusal is the session's spawn error, which the implementer retries and then sends to `needs_human`. The settings page shows which file is in force (section 6.4).

### 6.2 What it allows

- **Writing roles** (implementer, drafter, chat):
  - allow `Read(**)`, `Write(**)` (the workspace is the lane), `Shell(git)`, the code host's program (`Shell(gh)` on GitHub, `Shell(glab)` on GitLab, neither for a project with no remote; `hostCli`, [code-storage.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/code-storage.md) section 3.4), `Shell(ls)`, `Shell(cat)`, `Shell(grep)`, `Shell(rg)`, `Shell(find)`;
  - deny `Write(.git/**)`, `Write(.cursor/**)`, `Write(.retaskable/**)`, `Read(.env*)`, `Shell(rm)`, `Shell(curl)`, `Shell(wget)`, `Shell(ssh)`, `Shell(sudo)`.
  - Also `Shell(<program>)` for the program of each of the project's `checks` and gallery commands (e.g. `Shell(cargo)`, `Shell(npm)`), and the project's own `engines.allow`. Built: how a command becomes a program, and what never gets added, is section 6.4.
- **Read-only roles** (planner, scouts, reviewer, writer): the same list without `Write(**)`, and `--mode ask` as well as the allowlist.
- The computer's settings page lists the programs taken from the commands and lets the human add more (`engines.allow` on the project), never `Shell(*)`, which it refuses. Built: the entries and which file is in force are section 6.4.
- What a session asks that isn't allowed is refused by Cursor; the session carries on without it or says so in its result, as a Claude session does when auto mode refuses something. The prompt doesn't list the extra programs.
- Shell entries match by program, so `Shell(git)` lets `git push --force` through. **[Proposal]** The implementer prompt's existing rule (never push to the base branch) stays the guard there, as it is under Claude Code's auto mode; the base branch's own GitHub protection, if any, still applies.

### 6.3 The rules

agent.md section 2's **Permissions** principle and the repo's `CLAUDE.md` rules say (changed with this spec): Claude Code sessions always run with `--permission-mode auto`, never `--dangerously-skip-permissions` or `bypassPermissions`; Cursor sessions always run under the allowlist of this section, never `--force`; and orchestration uses only `claude -p` or `cursor-agent -p`, `git` and `gh`.

### 6.4 Extra allow entries

Built (`shared/cursorAllow.ts`, `allowlist` and `allowlistInForce` in `agent/src/engines/cursor.ts`). The base allow list, the deny list and "never `--force`" stay as they are (sections 6.2 and 6.3). Shell entries still match the program only, so `Shell(git)` still lets `git push --force` through; whether Cursor can match arguments is still open (section 12).

- **Where the extras are saved.** On the project, inside `engines`:

  ```json
  "engines": { "default": "claude", "roles": { "implementer": "cursor" }, "allow": ["Shell(make)", "Write(src/**)"] }
  ```

  Missing reads as none. Picking an engine keeps `allow` (`withDefault` and `withRole` keep the rest of `engines`). The machine's `cursor` (section 3.4) is a separate key.

- **Programs taken from commands** (`commandProgram` and `commandShells`): for each of the project's `checks[].command` and then `galleries[].command`, and nothing else (`playCommand` is run by the orchestrator, outside the session), the first word that isn't a `NAME=value` assignment, quotes stripped, becomes `Shell(<word>)`: `npm run typecheck` → `npm`, `CI=1 cargo test` → `cargo`, `"./gradlew" build` → `./gradlew`. Only the first word counts, so `cd shots && npx playwright test` gives `Shell(cd)` and not `npx`; such a gallery's program goes in the human's own entries. A denied program (`rm`, `curl`, `wget`, `ssh`, `sudo`) or a shell or launcher (`sh`, `bash`, `zsh`, `dash`, `ksh`, `fish`, `cmd`, `powershell`, `pwsh`, `env`, `xargs`, which would allow every command) is never added, so a check like `bash scripts/ci.sh` adds nothing; the orchestrator still runs that check itself, outside the session. Programs are compared by base name, without `.exe` and ignoring case, so `/bin/bash`, `bash.exe` and `PowerShell` count. Duplicates go. An empty command adds nothing.

- **What the human can add** (`allowEntryProblem`): `Shell(<program>)`, `Read(<glob>)` or `Write(<glob>)`. Refused, with the reason:
  - `Shell(*)` or `Shell(*…)`: "Shell(*) would allow every command";
  - a shell or launcher from the list above (`Shell(bash)`, `Shell(/bin/sh)`, `Shell(PowerShell)`): "Shell(bash) would allow every command";
  - a `Write` starting with `/`, `\`, `~` or a drive letter, or with a `..` path segment anywhere in it (`Write(docs/../../etc)`): "… writes outside the workspace";
  - a program the deny list names, compared the same way (`Shell(RM)`, `Shell(/bin/rm)`): "curl is always denied";
  - anything else: "… isn't Shell(…), Read(…) or Write(…)".

  The entries are typed separated by commas or new lines. A refused entry shows its reason, and the agent's `validate` refuses to save it ("Cursor can't be allowed this: …"). The Expo app draws the box (**Cursor may also run**); the agent only checks and saves.

- **What gets written** (`extraAllow`, passed as `SessionSpec.cursor.allow` and written by `prepareCursor`), only when Retaskable writes the lane's `.cursor/cli.json` (section 6.1): for a writing role, the base allow list, `Write(**)`, then the commands' programs, then the project's entries that pass the check, once each. A read-only role gets no extras at all. The deny list is section 6.2's, unchanged. A tracked `.cursor/cli.json` is still not overwritten and the extras aren't merged into it. The user's `~/.cursor/cli-config.json` is still only a veto.

- **Which file is in force** (`allowlistInForce(repo)`, read fresh each time the computer's settings page opens), shown as one line:
  - "Cursor follows the list Retaskable writes for each session." when the repo doesn't track `.cursor/cli.json`;
  - "Cursor follows this repo's own .cursor/cli.json, so the extra entries above don't apply." when it does;
  - and in red, "Cursor sessions won't start: .cursor/cli.json it allows Shell(*)" (or "isn't valid json", "lacks Shell(rm), …", or the user's file's reason) when a session would be refused.

  Above it, "Already allowed, from your checks and galleries: Shell(npm), Shell(cargo)", when there are any.

- **A session already running** keeps the file written when it started. The next session, including a follow-up, writes the list again from the config at that moment.

- **Tested** in `shared/cursorAllow.test.ts`: a command's program with assignments and quotes skipped; the checks' and galleries' programs once each, without denied ones; `Shell(*)`, shells and launchers (`/bin/bash`, `bash.exe`, `PowerShell`), outside writes, denied programs (`RM`, `/bin/rm` too) and other words refused. `cursor.test.ts`: a writing lane's file gets the extras and a read-only one doesn't; `allowlistInForce` says Retaskable's list for an untracked repo and the reason for a tracked `Shell(*)`; `cursorErrors` refuses `Shell(*)`. `engineChoice.test.ts`: the entries split on commas and survive picking an engine.

