## 3. Choosing the engine

### 3.1 Per computer and per role

Built. The choice is the computer's, not the server's: a team can mix Claude Code, Cursor, pi (section 13) and a local model. It is the machine key `engineChoice` in `.retaskable/config.json` (agent.md section 10), set in the tray's Settings…, `npm run link` or `RETASKABLE_ENGINES` ([agent-settings.md](../agent-settings.md) section 3.1), and `engineFor` (`agent/src/engines/choose.ts`) reads it when a session starts. Until the computer saves its own choice, a project's older `engines.default`/`roles` still count (`legacyChoice`). The machine-level `cursor.models` further down are built too (section 3.4). The computer reports its choice to the hub; the site only shows it (the Expo app's computer rows and Project settings).

- The computer gets an optional `engineChoice`:

  ```json
  "engineChoice": { "default": "claude", "roles": { "implementer": "local" } }
  ```

  Missing means everything on Claude Code, as today. An engine is `claude`, `cursor` or `local`. `local` is Claude Code with the model pinned to the local model (`Orchestrator.pinnedModel`): it falls back to the role's tier when the local model is off or down, waits for the local slot when busy, and a resumed session stays on the backend it started on. `roles` overrides `default` for these groups of roles (section 9):

  | Group | Roles (`Role` in `shared/types.ts`) |
  |---|---|
  | `planner` | `planner` |
  | `implementer` | `implementer`: roadmap slices, chores and playtest fixes |
  | `review` | `scout`, `reviewer` |
  | `drafter` | `drafter` (spec drafts) |
  | `chat` | `assistant` (Ask), `triager` |
  | `writer` | `writer` (release notes) |

- The machine-level settings have `cursor`: `{ "models": { "top": "gpt-5", "implementer": "auto", "scout": "auto" } }`, the Cursor model name per tier (section 3.2). Missing means `auto`, Cursor's own pick. The fields and the checks are section 3.4. It is its own key, not a field inside the project's `engines`, because a machine setting and a project setting can't share one object ([projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) section 3.1). The project's `engines.allow` is section 6.4.
- The engine is looked up when the session starts, in plain code (`engineFor(cfg, role)`), like the tier.

### 3.2 Models and the usage limit

Built. `--model` is the machine's Cursor model for the role's tier (section 3.4). A Cursor session counts toward `concurrency` and not toward `modelConcurrency` (`Slots`, kind `cursor`), and it never takes the local slot: a tier of `local` is recorded as `sonnet`. Auto concurrency's burn rate stays Claude Code's (`Slots.anthropic` leaves Cursor out). `waiting_for_limit` still holds Claude Code. A Cursor role keeps starting (`canStartCursor`): pause and a pending restart still apply, and the planner, chat and spec drafts still run while paused. The daily budget does not stop it (section 7). The computer reports **Waiting for limit** for the project, with `cursorCarriesOn` set when any role is on Cursor, so the site can say "Cursor roles carry on".

A Cursor result sets `limitHit` on a `429` or on an error whose text says "usage limit" or "rate limit". The step retries it after 30 minutes (`cursorLimitDecision`), and after 3 in a row the item goes to `needs_human`. Cursor gives no reset time, so there is no countdown. A session that finishes clears the row, a review included: when the final review succeeds, `cursorLimitHits` goes back to none, so the next implementer doesn't inherit a scout or review streak. The planner isn't an item: it waits the same 30 minutes, and that hold is checked before `git fetch` with the sync left requested, so the wait doesn't fetch on every poll and the planner runs once the hold ends. After 3, it stops retrying and shows the error.

- **Claude Code sessions** are exactly as today: the tiers of agent.md section 7.1 and the local model of section 7.2.
- **Cursor sessions** take their model from `cursor.models` by the tier their role would get (`top` for the planner, final reviewer, drafter and triager; `implementer` for implementers, asks and chores; `scout` for scouts and release notes), passed as `--model`. Built: the mapping, a bad name, and a follow-up are section 3.4. A Cursor session neither reads nor changes `state.usage`.
- **[Proposal]** So **a Cursor role keeps working while Claude Code waits for the limit**: `waiting_for_limit` holds only the sessions whose engine is Claude Code. Built, as above.
- **[Proposal]** Cursor sessions count toward `concurrency` (they're processes and lanes on this machine) but not toward `modelConcurrency`, and auto concurrency (agent.md section 7.1) leaves them out of its burn rate, which is Claude Code's. Built, as above.
- **[Proposal]** A Cursor session that ends with Cursor's own usage or rate limit (a `429`, or an error whose text says "usage limit" or "rate limit", matched in plain code) is treated like an interrupted session: its step retries it after 30 minutes, and after 3 in a row the item goes to `needs_human` saying so. Cursor gives no reset time, so there is no countdown. Built, as above.
- **Cursor's limit is the computer's, not the item's** (2026-10-07: Andrew's agent claimed item after item from the shared backlog while Cursor refused to run, and held five at `needs_human` while James's agents waited). A hit sets `Pool.cursorLimitedUntil` to 30 minutes on, and until then nothing starts on Cursor on that computer (`cursorMayStart`'s `limitedUntil`). New implementer work starts when the implementer's own engine can (`shouldStartNext(claudeMay, cursorMay, implementerOnCursor)`): Claude Code being free no longer starts work for a Cursor implementer, so no item is claimed only to hit the limit. An item that hits the limit with nothing pushed (no pull request) is given back to the shared backlog at once (`HostedWork.giveBack`, with "Given back: Cursor's usage limit was hit on <machine> before anything was pushed" in its history) for an agent that can work now; one with a pull request keeps the 30-minute retries above. On an agent's first fetch, items it holds at `needs_human` only for Cursor's limit with nothing pushed are given back the same way. Which agent takes the item next is the store's call, not this agent's. Tested in `agent/src/engines/limits.test.ts` and `agent/src/hostedWork.test.ts`.
- **Claude's sign-in is the computer's too** ([claude-signed-out.md](../claude-signed-out.md), built): a Claude session that ends signed out sets `Pool.claudeSignedOut`, which holds every Claude session on that computer (`canStartSession`, the runner's gate) while `canStartCursor` ignores it, so Cursor roles carry on, as they do while Claude waits for the usage limit.

### 3.3 Resuming

On a computer whose implementer isn't on Cursor, a build begun on Cursor elsewhere carries on in a fresh session on this computer's engine, on the same branch, and Cursor's limit there means carrying on with Claude at once, not waiting 30 minutes ([simple-flow.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/simple-flow.md) section 6).

- **A session resumes on the engine it started on**, as it resumes on the backend it started on today (agent.md section 7.1). Changing a role's engine applies to the next fresh session; an item's implementer that started on Claude Code keeps resuming there for its follow-ups. Built for the runner: `startedOn` reads the engine from the run's meta line, and the orchestrator passes it back.
- A Cursor session with no id yet gets one from `cursor-agent create-chat`, and a retry of a bad result resumes that chat. A fresh implementer run's prechosen id is not sent as `--resume`: `create-chat` replaces it, and the item keeps that chat id, written before `cursor-agent -p` is spawned so a restart resumes the chat and not the id chosen beforehand. A spec draft's chat id is kept the same way, before the process starts and again from the result, so a revision's `--resume` is that chat. A follow-up resumes it. When the item has moved to another lane, the next session starts fresh and picks up the branch or PR (`cursorFollowUp`). Tested end to end with a stubbed `cursor-agent` (`cursorRun.test.ts`).
- **[Proposal]** A Cursor chat isn't moved between lanes: when an item picks up again in a different lane, its next Cursor session starts fresh and picks up the branch or PR, as a Claude session does today when its transcript file can't be found (agent.md section 4.1). Built, as above. The lane is the one in the run's meta line.

### 3.4 Cursor model names

Built (`cursorModel` and `cursorModelProblem` in `shared/cursorAllow.ts`, `cursorModelFor` in `agent/src/engines/choose.ts`). It doesn't change Claude Code's tiers (agent.md section 7.1) or the rule that a Cursor session never falls back to Claude Code (section 4).

- **Where it's saved.** On the machine, at the top of `.retaskable/config.json`, beside `models`:

  ```json
  "cursor": { "models": { "top": "gpt-5", "implementer": "auto", "scout": "auto" } }
  ```

  `MACHINE_KEYS` in `agent/src/config.ts` has `cursor`, so it is one setting for every project ([projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) section 3.1). A project has no model names of its own. Missing, empty, or a config saved before this, reads as `auto`.

- **Which name a session gets**, looked up in plain code when it starts (`cursorModelFor(cfg, role)`, called by the orchestrator's runner wrapper and passed as `SessionSpec.cursor.model`), from the tier that role already uses:

  | Name | Roles | The tier it follows |
  |---|---|---|
  | `top` | `planner`, `reviewer`, `drafter`, `triager` | `topTier` (on Claude Code the drafter and triager follow `midTier`, agent.md section 7.1) |
  | `implementer` | `implementer` and a chore's implementer | `tier` |
  | `scout` | `scout`, `writer`, `assistant` | `scoutTier` (on Claude Code a `writer` runs on Haiku, agent.md section 7.1) |

  The name comes from the role alone, so a role whose Claude tier would be `local` still gets that row's Cursor name, never the local model's, and a tier of `local` is still recorded as `sonnet` (section 3.2). The 5-hour window doesn't change the three names.

- **Dollar caps** (2026-10-11, the human: "It shouldn't limit release notes either if I'm on a subscription"). The `budget.*Usd` caps (`--max-budget-usd`) apply only on an API key. On a Claude subscription the usage window is the limit, and a cap only stops a session short (an idea round was stopped at $0.53 before it wrote anything). So while Claude Code has reported a five-hour usage window in the last week (`onSubscription` in `agent/src/tiers.ts`, from the machine's saved `usage`), `SessionRunner` runs every session with `budgetUsd: 0` (`agent/src/engines/runner.ts`, wired in `pool.ts`), whatever the settings say. Tested in `tiers.test.ts`.

- **The chat** (2026-10-10, the human: "the agent should pick it up right away"). The logs showed every ask's session starting the moment the ask arrived; the time went to the session itself. So the `assistant` (Ask) session is light:
  - **Its tier is `scoutTier`** (Sonnet, or the local model near the limit), on Claude Code and on Cursor, not the implementer's `tier` (`assistant.ts`, `TIER` in `engines/choose.ts`).
  - **No MCP servers.** `SessionSpec.lean` leaves out even the project's own `.mcp.json`, so no server loads. They added 1–3 s to each start and their tools to each prompt.
  - **Every Claude session is strict about MCP** (2026-10-22): `buildArgs` passes `--strict-mcp-config` to all of them, so none of the person's own servers (mail, calendar, docs…) load, and passes the project's `.mcp.json` back with `--mcp-config` when it has one and the session isn't lean. `ENABLE_CLAUDEAI_MCP_SERVERS=false` in every session's environment turns the claude.ai connectors off too. Measured with `claude -p "say hi"` on Haiku: 144 tools and 6 servers before, 33 tools and none after, and a first turn of 35.2k tokens instead of 37.8k (the servers' tools are deferred behind tool search, so most of their weight never reached the prompt). Tested in `logic.test.ts`.
  - **A shorter state.** The agent asks the store for the project's open items and its latest 10 finished (the store's own read), not the project's whole history, and does not build or keep a local item list for the prompt.
  - **Its prompt** (`prompts/assistant.md`) answers a question from that state, reading only the few files that answer it when the state can't, and never surveys the repo.
  - **A request's plan shows its progress in the chat** ("Writing the plan", "Waiting its turn: 2 plans ahead"): the app draws that from the draft's session, not the agent.
  - **Tests**: `logic.test.ts` (the flag, only on a lean session), `cursor.test.ts` (the chat's Cursor name).

- **What `--model` is.** `cursorArgs` passes `--model <name>`, `auto` when there's none. The lookup is made for every session, so a follow-up (including a `--resume`) gets the name saved now. A process already running keeps the name it started with; nothing is killed to switch.

- **A name that isn't allowed.** A name is one or more of letters, digits, `_`, `.`, `:`, `/` and `-`, not starting with `-` (so `--force` can't be slipped in as a model). Saving refuses anything else: the agent's `validate` (`cursorErrors` in `agent/src/setup.ts`) says "Cursor model "a b" isn't a model name." and nothing is written. A bad name already in the file (edited by hand) is read as `auto`, so the session still starts, on Cursor's own pick. Retaskable keeps no list of Cursor's models; a name Cursor itself rejects comes back as that session's error, retried and then `needs_human` as any other failed Cursor session, and Claude Code is not substituted.

- **Where it is set.** On the computer's own settings page ([agent-settings.md](../agent-settings.md)): three boxes, **Cursor top model**, **Cursor implementer model** and **Cursor scout model**, each with placeholder `auto` and "Empty means auto, Cursor's own pick." A refused name shows in the form's errors after **Save**. On retaskable.com they are Project settings' **Cursor: top model**, **Cursor: implementer model** and **Cursor: scout model**, sent as the `settings_request`'s `cursorModels` ([cloud.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/cloud.md) section 6.1); the agent checks the name as above. The demo and the Agents tab's chips are the app's.

- **Tested** in `cursor.test.ts`: each role takes its tier's name; missing, blank or `--force` is `auto`; `--model` is the name in the spec; `cursorErrors` refuses a bad name. `shared/cursorAllow.test.ts`: a name, or `auto` when it's missing; `--force` and `a b` are refused.
