## 15. Smart and Fast models

[Proposal] (2026-10-29). Once built, this replaces picking an engine per kind of work and per lane (section 3.1), Cursor's tier names (section 3.4) and one engine taking over at the other's limit (section 3.5). Claude Code's tiers (agent.md section 7.1) become the step-down of section 15.5. The owner's thresholds on the site ([agent-settings.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/agent-settings.md) section 2.1) stay.

### 15.1 Decisions

- **[Decided]** (2026-10-29) The human: "Right now we set lanes and kinds of work by harness which i dont think is great. Lanes should just be a number, how many thats it. Kinds of work should be by model". Lanes are one number (section 15.6), and nothing is chosen by engine any more: a model names its engine.
- **[Decided]** (2026-10-29) The human: "its not that opus is better than GPT its whichever one you have more tokens with. but then again most users will only have 1 subscription." Models of one class are equals, and a session goes to the one with the most left (section 15.5). With one subscription a class is just that engine's model.
- **[Decided]** (2026-10-29) The human: "maybe we should auto categorize the models? like Opus and GPT top as "Smart" and Sonnet, etc. as "Fast"". Two classes, sorted by a catalog (section 15.3), and each kind of work takes a class in code (section 15.4). Nobody has to pick a model for a kind of work.
- **[Decided]** (2026-10-29) An engine that reports nothing about what's left comes last: it's used only when every engine that reports is low or at its limit.
- **[Decided]** (2026-10-29) A class still steps down as its tokens run low (Smart, then Fast, then the local model, then wait), so one subscription lasts the window, as today.
- **[Decided]** (2026-10-29) The human: "Add another type, escalation". A retry, a reopened item and a large item are their own kind of work, on Smart; a first try is Building, on Fast. It replaces `implementerModel`.
- **[Decided]** (2026-10-29) The human: "Maybe a basic mode and an expert mode?", then "write expert up now though". Basic is the default and the page has nothing else to set (section 15.7). Expert only adds overrides on top of Basic, never a second way of choosing (section 15.8).

### 15.2 The models a computer has

- **Found, not typed.** The list is built from the engines found and signed in on this computer (the setup checks, `probe.ts`), each giving its models:

  | Engine | Its models |
  |---|---|
  | Claude Code | `opus`, `sonnet` (and `haiku`, kept off the page: section 15.4) |
  | Cursor | its own list (`cursor-agent` models, section 15.11) |
  | Codex | the catalog's Codex models (Codex has no list) |
  | pi | the models signed in to in pi itself (`provider/id`) |

- **One row per model and engine.** Sonnet on Claude Code and Sonnet on Cursor are two rows. Basic shows a model only on its own engine (Claude models on Claude Code, GPT models on Codex, else on Cursor; anything else where it's found); Expert shows every row.
- **The local model** isn't in a class. It's the step-down below Fast (section 15.5), on while `local` is on and up, as today (agent.md section 7.2).
- **A model whose engine goes** (uninstalled, signed out) stays on the page as "Not found on this computer" or "Not signed in" and is skipped.

### 15.3 The catalog

`shared/models.ts` names each known model, its engine and its class, and sorts a name it doesn't know by pattern, so next month's names land in the right class without an update:

| Class | Matches |
|---|---|
| Smart | `opus`, `*-sol`, `gpt-N` with no suffix, `grok-N`, `gemini-*-pro`, `*-max` |
| Fast | `sonnet`, `mini` as a whole word (`gpt-6.1-mini`, not `gemini`), `*flash*`, `*-fast`, `haiku`, and anything not matched |

- The person's **Move** wins over the catalog, and is saved (section 15.9).
- `auto` (Cursor's own pick) is a Fast model on Cursor.
- **Built** (2026-10-29): `shared/models.ts` has `KNOWN_MODELS`, `classOf(name)` (a provider prefix and case are ignored; a name that says `mini`, `flash` or `-fast` stays Fast whatever else it matches, so `gemini-*` isn't mistaken for `mini`) and `ownEngine(name)` for Basic.

### 15.4 Kinds of work

Each kind takes a class, fixed in code in Basic:

| Kind | Roles (`Role` in `shared/types.ts`) | Class |
|---|---|---|
| Planning | `planner` | Smart |
| Reviewing | `reviewer` (the final review) | Smart |
| Escalation | `implementer` on a retry (`attempt` over 1), an item a try-out reopened, or size L or XL | Smart |
| Building | `implementer` on a first try: roadmap slices, chores and playtest fixes | Fast |
| Scouting | `scout` | Fast |
| Writing | `drafter`, `assistant` (Ask), `triager`, `writer` (release notes, ideas, vetting) | Fast |

- **The drafter is Writing**, not Planning: it writes from what it's given, and Opus is kept for the planner and the final review (agent.md section 7.1, `midTier`, 2026-10-22).
- **Text-only work on Claude Code runs on Haiku.** A `writer` session whose pick is Claude Code runs `haiku`, as `writerModel` does today. Haiku is never a row on the page and never picked for anything else: Claude Code runs it in `default` permission mode, which refuses most commands in `claude -p` (agent.md section 7.1).
- `kindFor(role, item)` in `agent/src/engines/choose.ts`, plain code.
- **Built** (2026-10-29): `kindFor` in `choose.ts`, with the same test as `implementerModel` for Escalation. `Kind` is in `shared/types.ts`.

### 15.5 Which model a session gets

Looked up when the session starts, after it gets its slot, in plain code (`modelFor(cfg, kind, readings)`):

1. **The candidates** are the kind's class's models (Basic) or the kind's own models (Expert, section 15.8) that are turned on, whose engine is found and signed in and may start (`canStartOn`: pause, a pending restart and its own limit for every engine; the daily budget, the 5-hour window and `laneScaling` for Claude Code), and whose share left is above the kind's step-down threshold below.
2. **Most left wins.** The share left is 100 less the used percentage of the engine's tighter window: Claude Code's 5-hour or weekly, whichever has less left (`state.usage`); Codex's tighter of `primary` and `secondary` (section 15.11's reading); Cursor's billing period (`cursorUsage.ts`, section 7). Two models on one engine share its reading.
3. **No reading comes last.** An engine that reports nothing (pi), or whose reading failed or is older than its window, ranks below every engine that has one. Among those, the one with fewer sessions running goes first. A tie goes to the order of the page.
4. **Step down** when no candidate is left. Each kind has a chain, with the owner's thresholds (`models.*Pct`, the site's) as its numbers in Basic:

   | Kind | Its class while left is at least | Then | Then |
   |---|---|---|---|
   | Planning, Reviewing | `pauseBelowPct` (5%) | the local model | wait |
   | Escalation | `opusAbovePct` (50%) | Fast down to `sonnetAbovePct` (20%) | as Building |
   | Building, Scouting, Writing | `sonnetAbovePct` (20%) | the local model, else Fast down to `pauseBelowPct` | wait |

   A threshold applies only to an engine that has a reading; one with none stays a candidate until it hits its limit. **Wait** is `waiting_for_limit` until the earliest reset among the kind's models, as today.

- **A resumed session stays on the engine and model it started on** (section 3.3, agent.md section 7.1); the pick is only for a fresh session.
- **Slots** are as today: a Claude Code session counts toward `concurrency` and `modelConcurrency`, any other engine's toward `concurrency` only (section 3.2).
- Each session records its engine and model, shown on its panel and in the transcript, as today.
- **The other engine takes over at a limit** (section 3.5) falls out of this: a session that ends at its engine's limit is retried, and the retry's pick skips that engine while it's out.
- **Built** (2026-10-29): `modelFor(input)` in `agent/src/engines/modelFor.ts`, pure, not yet called. It takes a `role` too, for Haiku. A chain's stage is the models to try while at least its number is left ("at least", so exactly 20% is still Fast), then the local model, then the next stage; Expert's steps make the same chain with its own numbers.

- **Built** (2026-10-29, 2 of 3): a new session asks `modelFor` through `pickNow` (`agent/src/enginePick.ts`, inputs from `engines/pickInput.ts`) and runs on its engine with its model: Claude Code keeps its own tier-driven model in Basic, so a Claude-only computer is unchanged; Codex gets `-m`, pi `--model`, Cursor the name. A resume stays on the engine it began on (a limit on that engine starts it fresh on the new pick); a `wait` pick goes to the waiting_for_limit path. Slots are counted as before.

### 15.6 Lanes

- **Lanes are one number** (`lanes`, [agent-settings.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/agent-settings.md) section 3.1), and a lane has no engine of its own. Whatever model a session in it gets is the lane's for that session.
- **[Decided]** (2026-10-29) **Until set, the number comes from the plan.** The human: "we need auto lane limits based on plans if we can get them, like if they are on Claude Max 20x they should default to 4 lanes and 5x 3 lanes and the lower one just 2 or 1 lane."

  | Claude plan (`rateLimitTier`) | Lanes |
  |---|---|
  | Max 20x (`*_max_20x`) | 4 |
  | Max 5x (`*_max_5x`), or Max with any other tier | 3 |
  | Pro (`subscriptionType` `pro`) | 2 |
  | Free | 1 |
  | No reading: an API key, not signed in, or a file that doesn't read | 2, as before |

  - **Where the plan comes from.** Claude Code's sign-in file, `~/.claude/.credentials.json` (`claudeAiOauth.subscriptionType` and `.rateLimitTier`; `CLAUDE_CONFIG_DIR` moves it). It's undocumented, so like Cursor's reading (section 7) anything unexpected is "no reading", never an error. `claudePlan()` in `agent/src/engines/claudePlan.ts` reads those two fields and nothing else: the tokens beside them are never kept, logged or sent. Seen 2026-10-29 with `default_claude_max_5x`; `claude auth status` gives only `subscriptionType` (`max`), not 5x or 20x.
  - **Live, until set.** `lanes` missing means auto: read again when the agent starts and every hour, so an upgrade adds a lane without a restart (a lane past the new number finishes its session first, as when the number is lowered by hand). A number the person saves, or `RETASKABLE_LANES`, wins and stays. Setup offers auto and says the number it found.
  - **The page** shows "Auto: 3 (Claude Max 5x)" with a field to set it; clearing the field goes back to auto. The site's `computerRows` show the same words.
  - **Only Claude Code's plan counts** for now: a computer with Claude Max 5x and Codex still gets 3. A plan for Codex or Cursor adding lanes is section 15.11.
  - A config saved before this keeps its `lanes`: it was set by someone (or by setup's old default of 2), and it isn't changed behind their back.
  - **Built** (2026-10-29): `claudePlan()`/`lanesForPlan()` in `agent/src/engines/claudePlan.ts`; a `max` plan whose tier is neither 5x nor 20x counts as Max 5x (3 lanes). `Pool.configure` fills a missing `lanes` in the running config only (`withAutoLanes`), and `Pool.lanesAuto` (`{ lanes, plan }`) is what the page shows; the plan is read again hourly and the pool configures again when it changed, as for a hand-lowered number. 
  - **Built** (2026-10-29, 3a): the page and setup. `currentSettings` gives `lanes` (what runs) and, while none is set, `lanesAuto` (`lanes`, `plan`, and `text`: "Auto: 3 (Claude Max 5x)", or "Auto: 2" with no plan reading), from `Pool.lanesAuto`; it is never written back as a set value. The page's field is empty with that text as its placeholder and hint; clearing it, or leaving it empty, saves `lanes: null`, which removes `lanes` from the file so the lanes follow the plan; a number typed is saved as set, even the one auto gives now. Setup's lanes question defaults to `auto` (Enter saves nothing), says what auto is now, and takes `auto` or a number from 1 to 64. Not yet: the site's `computerRows`.
- **A free lane claims new work only when Building's pick may start** (`laneMayStart` reads `modelFor`, not a lane engine), so nothing is claimed only to hit a limit, as section 3.2 has it now.
- `laneScaling` (Claude Code's 5-hour window holding lanes back) counts only sessions on Claude Code, as today.

- **Built** (2026-10-29, 2 of 3): lanes have no engine of their own any more (`laneEngine` is no longer read); `laneMayStart` asks Building's pick, and a lane running a session on another engine is exempt from laneScaling because of what runs in it.

### 15.7 Basic

The settings page's **Who does what** becomes **Models** ([agent-settings.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/agent-settings.md) section 4.5):

```
Lanes   [ 4 ]

Smart   Opus · Claude Code          62% left      [Move ▾] [Turn off]
        GPT-6.1 Sol · Codex         not reported  [Move ▾] [Turn off]
Fast    Sonnet · Claude Code        62% left      [Move ▾] [Turn off]
        GPT-6.1 mini · Codex        not reported  [Move ▾] [Turn off]

Planning, reviews and retries use Smart. Building, scouts and writing use Fast.
Each takes whichever has the most left.                          [ Expert ]
```

- Each row says what's left, "not reported", "At its limit until 3:40 pm", "Not signed in" or "Not found on this computer".
- **Move** puts a model in the other class; **Turn off** keeps it from being picked. Both are saved with the page's one **Save**.
- A computer with only Claude Code sees Opus and Sonnet, already right, and needn't touch anything.

### 15.8 Expert

The **Expert** switch shows the same page with more under it. It changes nothing by itself: a kind with no override follows Basic, so Expert starts out exactly as Basic.

- **Each kind of work** (the six of section 15.4) can use Smart, Fast, or models picked from the list (every row, any engine). Picked models are equals, as a class's are.
- **Each kind's step-down**: up to three steps, each "below N% left, use Fast / Smart / the local model / wait". A kind that sets its own uses its own numbers in place of the site's thresholds, and the site says so (section 15.10).
- **Every row** of section 15.2: a model on each engine it runs on, so Sonnet can run on Cursor.
- **Back to Basic** ignores the overrides but keeps them, so turning Expert on again restores them. The page says "Your Expert choices are kept but not used."
- An override naming a model that isn't found is skipped, as in section 15.5, and the page marks it.

### 15.9 Where it's saved

The machine key `work` in `.retaskable/config.json` (`MACHINE_KEYS`), with only what was changed:

```json
"work": {
  "expert": false,
  "models": { "codex:gpt-6.1-mini": { "class": "smart" }, "cursor:auto": { "off": true } },
  "kinds": {
    "escalation": {
      "use": ["claude:opus", "codex:gpt-6.1-sol"],
      "steps": [{ "belowPct": 50, "to": "fast" }, { "belowPct": 20, "to": "local" }, { "belowPct": 5, "to": "wait" }]
    }
  }
}
```

- A model is `engine:name`. `use` is `"smart"`, `"fast"` or a list of models; `to` is `smart`, `fast`, `local` or `wait`. Thresholds are whole numbers from 0 to 100, each step's below the last.
- **Environment** (section 3.2 of agent-settings.md): `RETASKABLE_ENGINES` becomes the engines that may be used (`claude,codex`; missing is every engine found). `RETASKABLE_MODELS` sets `work.models` as `codex:gpt-6.1-mini=smart,cursor:auto=off`. A value that doesn't read is left out whole, and the page greys out what a variable sets.
- `agent/src/machineSettings.ts` checks it as it checks the rest; the page refuses what doesn't check.
- **Built** (2026-10-29): `WorkConfig` in `shared/types.ts`, `work` in `MACHINE_KEYS`, `workProblem` (the checks: ids are `engine:name` on a known engine, thresholds whole 0 to 100, each below the last, at most three steps, `use` not empty) and `workModelsOf` (`RETASKABLE_MODELS`, whole or not at all) in `machineSettings.ts`. In a kind's `use`, the entry `local` stands for the local model (section 15.10). Not yet called from the settings page or the loader.

- **Built** (2026-10-29, 2 of 3): `workSettings` is saved by `applySettings` (`work`, checked by `workProblem`, which drops `engineChoice` and `cursor.models` from the file); `RETASKABLE_MODELS` (`workModelsOf`) adds model rows; `RETASKABLE_ENGINES=claude,codex` (the new form) only limits the engines found (`allowedEngines`).

### 15.10 Moving over

Read once when the agent starts, noted once (`retiredKeysNote`) and dropped by the next save:

- **Nothing set, or everything on Claude Code**: Basic, and nothing is saved. Such a computer runs exactly as before, apart from Escalation and the drafter's tier, which already match today's rules.
- **Any kind of work on another engine, or `cursor.models` set**: Expert, with each kind using its old engine's models of the kind's class: a role group on Cursor uses Cursor's model for that tier (`top` to Smart, `implementer` and `scout` to Fast), on Codex the catalog's Codex model of the class, on `local` the local model.
- **Built** (2026-10-29): `workFromLegacy(engineChoice, cursor)` in `agent/src/engines/workFromLegacy.ts`, pure, not yet called. A group on the local model becomes `use: ["local"]` (clean, since a step "below 100%" would not pin it). Where a kind's groups disagree (Writing has three) it uses all of their models as equals, Claude Code's as its class model (`claude:opus`, `claude:sonnet`). A group on pi leaves its kinds to Basic (pi has no model to name). All-Claude with only `cursor.models` set saves nothing, since it would behave as Basic.
- **`engineChoice.lanes`** (lanes per engine) is dropped. A computer that split its lanes between Claude Code and Cursor gets Building on both, and most-left balancing does what the split did. The note says so.
- `RETASKABLE_ENGINES` in the old forms (`implementer=local`; `lanes=…` is ignored) and `RETASKABLE_CURSOR_MODELS` are read the same way, and the page says to change them to the new forms.
- **The site** (`computerRows`, [agent-settings.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/agent-settings.md) section 2.2) shows Basic or Expert, each class's models with what's left and which are off, and in Expert each kind that sets its own. The engines and Cursor's models rows go. It is still set only at the computer (`retiredRefusal`), and a computer too old to send `work` shows its old rows.

- **Built** (2026-10-29, 2 of 3): when a Codex session ends, `Pool.readCodexUsage` reads the log by the thread id and keeps it in `machine.json` (`codexUsage`, shown as `usage.codex`); `codexReading` feeds it to the pick.

### 15.11 Open questions

- **Codex's reading: answered** (2026-10-29, codex-cli 0.165.0, signed in with ChatGPT). `codex exec --json` doesn't print it: its `turn.completed` has only token counts. Codex's own session log does: `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<time>-<thread_id>.jsonl` (`CODEX_HOME` is `~/.codex` when unset), found by the `thread_id` of `thread.started`, has a `token_count` event whose `rate_limits` is `{ primary: { used_percent, window_minutes, resets_at }, secondary, plan_type, rate_limit_reached_type }`. Seen on a free plan: `primary` 0% of a 43200-minute (30-day) window, `secondary` null, `plan_type` `free`. After each Codex session `codexUsage()` (`agent/src/engines/codexUsage.ts`) reads the last such event from that file and keeps only those fields, as `state.usage.codex`. Anything unexpected is no reading, never an error. A paid plan's windows (expected: 5 hours and a week) are taken as they come, by `window_minutes`. Before the first Codex session on a computer there is no reading, so Codex comes last until it has run once.
**Built** (2026-10-29): `codexUsage(threadId, env, home)` reads the file (the newest three day folders); `Pool.readCodexUsage` calls it with `thread.started`'s id when a Codex session ends (2 of 3).
- **Cursor's reading is only taken while the settings page asks for it** (`CURSOR_PLAN_EVERY_MS`). It would be read every 5 minutes while a kind in use has a Cursor model, and never otherwise.
- **Cursor's and pi's model lists**: whether `cursor-agent` and `pi` can list the models a person can use, or the catalog has to stand in, as for Codex.
- **The plan on macOS.** Claude Code keeps its sign-in in the Keychain there (`Claude Code-credentials`), not in the file, and reading it can ask the person to allow it. Until that's checked, a Mac has no reading and gets 2 lanes.
- **Other engines' plans.** Codex's comes with its reading (`plan_type`: `free` seen; `plus`, `pro` expected) and Cursor's dashboard may say Cursor's. How many lanes a second subscription adds is still open.
- **A month against 5 hours.** Cursor's period is a month and Claude Code's window 5 hours, so 40% of Cursor's month used beats 70% of Claude's window used, though Claude's resets sooner. Taken as is for now.

### 15.12 What goes

`engineFor`'s groups (`GROUP`, `ENGINE_GROUPS`, `pickFor`), `laneEngine` and `hasLaneEngines`, `cursorModelFor` and `TIER`, section 3.5's takeover as its own rule, `implementerModel` (Escalation), `midTier` (the drafter is Fast), and Codex's "no `-m`" (it gets its model).

- **Built** (2026-10-29, 2 of 3): on load `projectConfig` reads an old `engineChoice` or `cursor.models` as Expert `work` on the running config only (`workFromLegacy`), and `retiredKeysNote` says so at start-up.
- **Built** (2026-10-29, 2c): `engineFor`, `pickFor`, `GROUP`, `laneEngine`, `hasLaneEngines`, `pinnedLocal`, `cursorModelFor` and `TIER` are gone, and so are the lane engines of the settings page (`engines.lanes`, the lane selects, `lanes=`) and `ComputerLane.own`. `usesClaude`, `usesCursor`, `usesEngine`, `usedFor` and `implementerMayUseCursor` now read `work` (`choose.ts`): the engines a kind's Expert list names, else Claude Code. Kept: `ENGINE_GROUPS` and `legacyChoice`, which `workFromLegacy` and the load of old config read (the page no longer edits `engineChoice`). Not yet (3 of 3): the settings page's models and `computerRows`, `laneEngines` on the wire (cloud and app read it if present).
- **Built 3c** (2026-10-29): Claude Code's model in Basic comes from the pick too. `pinnedModel` asks `pickNow` again when the slot is granted (so usage then decides); the local model is the pick's when it is on and up, or the caller's when it asks (`spare`); a resumed session keeps the model it started on. `implementerModel` and `midTier` are gone, and the callers pass only a fallback tier. Behaviour that moves: the triager's second look (`triage-check`) was Opus and is Fast (Sonnet), as section 15.4 has the triager; the share left is the tighter of the 5-hour and weekly windows, not the 5-hour alone. At 80%, 30% and 10% left the planner is Opus, a first try Sonnet, and a retry or size L Opus, Sonnet, Sonnet, as before (a Fast kind below 20% takes the local model first when it is on).

- **Built 3b** (2026-10-29): the settings page's Models in Basic and Expert (agent-settings.md section 3.1) replaces the per-group selects, and setup's engines question is gone. Saving `work` drops `engineChoice`. Not yet: `computerRows` (the site), the env forms' greying on the page beyond locking, and the marking of a named-but-missing model beyond "(not found)" in a kind's list.

- **Built 3d** (2026-10-29): the site. `hello.computer.lanesAuto` (`{ plan? }`, the plan's display name) is sent while the lane count follows the Claude plan, and `computerRows` reads "3 lanes (auto, Claude Max 5x)"; `usage.codex` (`CodexUse`) goes up in the page view and snapshot like Cursor's, and shows on the Agents page's Usage card ("42% of its 5-hour window used, resets 14:05 · 12% of its weekly window used") and as the Codex row's bar in the computer's status. `laneEngines` is gone from `ComputerView`, `ProjectComputer`, the hub's answer and the team list (a lane shows no engine); the hello's schema passes unknown keys, so an old agent's still parses. Both fields are optional, so an older agent or site keeps working.

### 15.13 Build order and tests

1. **The agent** (about 500 lines): the catalog, `work` and its checks, moving over, `kindFor` and `modelFor`, `laneMayStart`. Tested: the catalog's patterns; most left wins; no reading comes last and fewer sessions breaks its tie; each kind's step-down at each threshold; Escalation for a retry, a reopened item and size L; Haiku only for `writer` on Claude Code; Basic ignores the overrides; each moving-over case; `claudePlan` for 20x, 5x, Pro, free, an API key and a file that doesn't read, and that it keeps no token; `codexUsage` from a recorded rollout file (free plan, one window), one with two windows, and one with no `token_count`.
2. **The settings page and the site**: Models in Basic and Expert, `computerRows`, the env forms. Tested: Save writes only what changed; Back to Basic keeps the overrides; a bad step order is refused.
