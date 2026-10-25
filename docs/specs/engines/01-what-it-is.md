## 1. What it is

Today every Retaskable session is a Claude Code session (`claude -p`). This step adds Cursor's CLI agent (`cursor-agent -p`) as a second **engine**, beside Claude Code and not instead of it. Which engine a session runs on is chosen per project, and per role within a project: for example the implementer on Cursor while the planner, the review and the chat stay on Claude Code.

From the human's request: "Support Cursor's CLI agent (cursor-agent -p) in addition to Claude Code (not a full replacement), selectable per project and ideally per role (e.g. implementer on Cursor, reviewers/planner on Claude)."

- **[Decided]** Claude Code stays, and stays the default. Cursor is an extra engine, chosen per computer and per role, with the local model as a third choice (section 3).
- **[Decided]** An engine layer goes around `agent/src/claude.ts` first, with no change in behaviour (section 2, slice 1).
- **[Decided]** Cursor has no `--json-schema`, so a Cursor session's structured result is taken from its final message, checked against the same schema as a Claude session's, and asked for again when it's missing or wrong (section 5).
- **[Decided]** Cursor reports no cost per session, so the budget gets an **unknown cost** mode (section 7).
- **[Decided]** Cursor sessions never run with `--force`. They run under a safe per-project allowlist written as `.cursor/cli.json` (section 6), and agent.md and the repo's `CLAUDE.md` rules say so.
- **[Decided]** A Cursor session's live transcript shows in the same transcript view as a Claude session's, from Cursor's `stream-json` events (section 8). It is streamed live and never stored in the cloud.
- The orchestrator's control flow doesn't change: which engine runs a session is a lookup in plain code, never a model's choice, and both engines answer through the same result schemas (agent.md section 2).
- Cursor's model names (section 3.4) and the extra allow entries (section 6.4) don't change anything marked decided above, and they add no command.
