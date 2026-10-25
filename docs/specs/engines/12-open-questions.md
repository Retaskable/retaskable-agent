## 12. Open questions

- **[Proposal]** Does `cursor-agent` run on native Windows (not WSL) well enough for lanes, junctions and long paths? Checked first in slice 2; if not, Cursor is offered only where it runs (section 4).
- Do Cursor's `Shell(...)` entries match the program alone, or can they match arguments (e.g. deny `Shell(git push --force)`)? If they can, section 6.2's deny list grows. Section 6.4 assumes the program alone, which is what section 6.2 already says, until this is known.
- Can a Cursor chat be resumed from another folder, as Claude Code's can by copying its transcript (agent.md section 4.1)? Until it's known, it starts fresh (section 3.3).
- Should a role fall back to Claude Code when Cursor is down or at its limit, or the other way round? For now neither: a session runs on its role's engine or waits (sections 3.2, 4).
