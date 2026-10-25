## 13. The pi engine

Built. [Pi](https://pi.dev) (`pi`, the pi coding agent) is a third engine beside Claude Code and Cursor, picked per computer and per role like the others (section 3.1: `claude`, `cursor`, `pi` or `local`). It is `agent/src/engines/pi.ts`.

- **Running it.** `pi --mode json --no-session --model <model>`, with the prompt on stdin. The binary is `RETASKABLE_PI`, else `pi` on the PATH. A read-only session (planner, reviewer, scout) adds `--tools read,grep,find,ls`; pi has no sandbox, so a writing session runs with its own tools in the lane's folder.
- **The model.** The role's model name goes to `--model` as is, and pi matches it (a name like `sonnet`, or `provider/id`). Which providers work is whatever the person has signed in to in pi itself (`pi auth`).
- **The result.** As for Cursor (section 5): the prompt asks for one fenced `json` block at the end of the final message, and the reader checks it against the result schema when pi prints `agent_settled`. A miss is `invalid_result` and is retried with the corrective prompt. A failed model call (an assistant message with `stopReason: "error"`) reports pi's `errorMessage`.
- **No resume.** `--no-session` means the runner owns the transcript file and the session id is that file's id. A follow-up never resumes pi's conversation: an implementer follow-up on pi, or on a build that began on pi, starts a fresh session on the same branch (section 3.3).
- **Cost.** Pi reports none the Retaskable way, so a pi session counts as unknown cost (section 7), like Cursor's, with no budget cap. Time is bounded only by the quiet-session watchdog; the Cursor minutes cap does not apply.
- **Transcript.** `message_end` text and tool starts show as events (section 8); `auto_retry_start` shows as "Retrying".
