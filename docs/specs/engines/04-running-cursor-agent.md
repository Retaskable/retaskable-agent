## 4. Running `cursor-agent`

Built (`cursorArgs`). A Cursor session is started like this, with the lane's worktree as cwd and the prompt on stdin, as Claude Code's is. `--model` is the role's Cursor model (section 3.4), `auto` by default. A read-only role also gets `--mode ask`, and every session gets `--trust` so a headless run isn't stopped on the workspace-trust prompt. Still never `--force`.

```
cursor-agent -p \
  --output-format stream-json \
  --trust \
  --model auto \
  --mode ask \
  --resume <chatId>
```

- **Never `--force`.** What a session may do comes from the allowlist (section 6), never from turning permission checks off.
- **[Proposal]** No `--max-budget-usd` exists for Cursor, so the runaway guard is time: that session's own project's `budget.cursorMaxMinutes` (default 60), after which it's killed like the watchdog's silent session (agent.md section 7) and resumed, up to 3 times. Built (`runningLongerThan`, subtype `cursor_time`): each running Cursor session is judged on its project's minutes, not the first project's. The watchdog's 20 silent minutes apply too, to every engine.
- **Read-only roles** (the planner, scouts, the reviewer, release notes) get the read-only allowlist (section 6.2) in place of `--disallowedTools`.
- **No sandbox.** Claude Code's sandbox (agent.md section 4.2) is Claude Code's own; a Cursor session runs unsandboxed on every platform, with the allowlist as its guard. Setup says so beside the engine choice.
- Signing in to Cursor is the human's job (`cursor-agent login`, or `CURSOR_API_KEY` in the agent's environment); Retaskable never stores a Cursor key. Built: Setup's probe (`probe.ts`) runs `cursor-agent --version` and `cursor-agent status` and greys Cursor out when either fails, saying which. `resolveCursor` uses `RETASKABLE_CURSOR` (or its old name, [retaskable.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/retaskable.md) section 7.3), else `cursor-agent` on `PATH` (and `cursor-agent.cmd` on Windows), else `~/.local/bin`.
- **[Proposal]** Native Windows: the agent is Windows-first (the repo's `CLAUDE.md`). If `cursor-agent` doesn't run natively on a machine, Setup doesn't offer Cursor there and says why; a config naming Cursor on that machine fails the session with that reason, which goes to `needs_human` rather than falling back to Claude Code silently. That's the probe: a binary that isn't there, or that doesn't answer, is "isn't installed" or "isn't signed in", and a session whose binary won't start fails with that spawn error rather than falling back to Claude Code.
