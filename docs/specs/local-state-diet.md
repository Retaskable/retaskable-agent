# A state file that stops growing — Spec

Status: v0.3 (2026-11-02: section 3.1 added, every new line there a **[Proposal]**), backlog id local-state-trim. From an audit of Retaskable on 2026-10-11 (the agent's `.retaskable/projects/<id>/state.json` is 8 MB for Retaskable and grows forever). Section 2's **[Decided]** line is what the human asked for; the **[Proposal]** lines fill the gaps and are the human's call. Trimmed for the agent: the item and draft records that made the file grow (timelines, merged drafts' files) are the shared backlog store's now, so only what is the computer's own is left here. Changes where each project's state lives in [projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) section 4 and the cleanup in [last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.2.

Theme: Orchestration

Kind: change

## 1. What it is

The agent keeps each project's own state in `.retaskable/projects/<id>/state.json` (`Store`, `agent/src/store.ts`). For Retaskable the file was 8 MB, and nothing ever made it smaller:
- **Every save rewrote the whole file pretty-printed**: `writeJsonAtomic` used `JSON.stringify(data, null, 2)`, and `Store.save` ran on every change.
- **It held copies of what the store keeps**: merged drafts' `files` (some about 400 KB each) and items' and drafts' timelines (one item had 462 entries). Items and drafts live only in the store ([last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.7), so the agent keeps no local copy of them to grow, and no cleanup of such copies is the agent's to do. The agent asks the store for an item's or draft's record and timeline, and writes timeline notes to the store.

## 2. Decisions

- **[Decided]** The human asked for this fix on 2026-10-11 after the audit: the file stays small, and nothing the UI shows changes.
- **[Proposal, built]** The file is written compact: no indentation. So is `machine.json`; the files people may read or edit (`config.json` and the rest) stay indented.
- **[Proposal, built]** A one-off trim runs on load, so an existing file shrinks the first time the new agent starts. With no item or draft records kept locally, that trim only drops whatever of them an older agent left behind.

## 3. Behavior

- **Compact JSON**:
  - `writeJsonAtomic(file, data, compact)` writes `JSON.stringify(data)` when `compact`, indented otherwise;
  - `Store.save` and `MachineStore.save` pass `compact`, so `state.json` and `machine.json` are compact; nothing reads them by line;
  - every other file it writes (`config.json`, the `*.offered.json` files and the rest) stays indented.
- **The one-off trim on load** (`Store`'s constructor):
  - after reading the file, a pure `trimState(data, now)` drops any item and draft records an older agent kept, answering whether it changed anything;
  - when it did, the store saves at once and logs "state.json trimmed from <a> MB to <b> MB (<file>)" to the agent's log;
  - it is safe to run on every load: a trimmed file comes back unchanged.
- **Spec words**: [projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) section 4 says the file is compact.

## 3.1 What the file holds, key by key

Every line here is a **[Proposal]** unless it says "as built". It fills a gap: the sections above say what to drop, never what is left or how big it may get, and the run-loop work ([pieces.md](pieces.md) section 3.1: "`state.json` keeps only the computer's own settings and caches: no runs, no tokens, no items") changes what is left.

**As built, a project's `state.json` holds six keys** (`src/store/project.rs`); a key it doesn't know round-trips untouched, so a newer or older agent's keys survive a save:

| Key | What it is | Bound |
| --- | --- | --- |
| `usage` | The latest 5-hour usage reading ([agent/07.1-model-tiers.md](agent/07.1-model-tiers.md)) | One value; replaced, never appended |
| `smoke` | The post-merge smoke's latest result (`sha`, `status`, `at`, `url`, `tail`) | One record; replaced on each smoke |
| `leases` | Lease token per item this computer holds | One entry per held item, so at most the number of lanes |
| `unreleased` | Give-backs the store hasn't taken yet: token and note | One entry per item; removed once the store takes it |
| `unparked` | Leases let go while an item waits, not yet taken | One entry per item; removed once the store takes it |
| `runs` | Each leased item's state, lane, session id, branch and slice (`RunRecord`) | One entry per leased item |

**When the run-loop lands** ([pieces.md](pieces.md) sections 3.1 and 4), the last four keys have no owner: the Rust agent never claims an item, holds no lease and resumes no session. Then:
- `leases`, `unreleased`, `unparked` and `runs` join the legacy keys (`items`, `drafts`) that the one-off trim drops on load, in the same pass, with the same "state.json trimmed" log line when it changes anything.
- Nothing is sent to the store for what is dropped. A give-back that was still waiting is not retried: the old lease runs out in the store, and `runs_drop` on start ends any run this agent had.
- What stays is `usage` and `smoke`, and whatever caches later items add.

**A key must name its bound.** A spec that adds a key to `state.json` or `machine.json` says in the same place how big it can get and what removes old entries (a count, a window of days, or "one value, replaced"). A list that only ever appends is not allowed there; `machine.json`'s `codexTokens`, which keeps only the last week, is the model.

**A file that is too big or broken, and what happens:**
- After the trim, a file over 1 MB logs a warning with each key's size, largest first, so a runaway key is found by reading the log. It changes nothing else: no second trim, no cap on the file.
- A file that isn't valid JSON stops that project from starting, with the file named in the error. It is not replaced by an empty one, so a person can look at it; this agent works for a shared backlog and fails closed ([README.md](README.md)).
- A file whose top level isn't an object loads as empty (as built), and is written back as `{}` only when something is next saved.

## 4. Tests

- `agent/src/store.test.ts`: loading a file with old item and draft records leaves none of them; the saved file has no newline before its first key, and loading it again leaves it as it is.

- **[Proposal]** `src/store/project.rs`: a file with `leases`, `unreleased`, `unparked` and `runs` loses all four once the run-loop lands and keeps `usage` and `smoke`; an unknown key survives a load and save; a file over 1 MB logs its largest keys; an invalid file errors naming the path and is left as it was.

## 5. Acceptance

- Starting the new agent on Retaskable's own project logs the trim, and `state.json` drops from about 8 MB to under 1.5 MB.
- A week later the file hasn't grown beyond the computer's own state.
- `npm test` passes.
- **[Proposal]** After the run-loop lands, a `state.json` from today's agent has only `usage` and `smoke` after its first start, and stays under 100 KB over a week.
