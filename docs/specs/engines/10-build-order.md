## 10. Build order

cursor-engine (its item is in the backlog store), in slices:

| Slice | What |
|---|---|
| **1** | **The engine layer** (section 2). Built |
| **2** | **The Cursor engine.** Built, for every role group, not only chat and spec drafts: finding and checking `cursor-agent`, its arguments (section 4), the allowlist and its checks (section 6), the transcript mapping (section 8), the result from the final message with its retries (section 5), and `engines` in the config. Setup's picker landed here too (section 9). Resuming an implementer's chat is slice 3 |
| **3** | **The implementer on Cursor.** Built: roadmap slices, chores and playtest fixes resume the Cursor chat (section 3.3), a fresh chat when the lane changed, the time guard, Cursor's own limit (section 3.2), and Cursor roles carrying on while Claude Code waits. Tested end to end with a stubbed `cursor-agent` |
| **4** | **The rest of settings, and unknown cost**. Unknown cost is built (section 7): `costUsd: null` reported to the store. Cursor model names (section 3.4) and the extra allow entries with which file is in force (section 6.4) are built too. |

