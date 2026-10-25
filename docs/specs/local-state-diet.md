# A state file that stops growing — Spec

Status: v0.2, backlog id local-state-trim. From an audit of Retaskable on 2026-10-11 (the agent's `.retaskable/projects/<id>/state.json` is 8 MB for Retaskable and grows forever). Section 2's **[Decided]** line is what the human asked for; the **[Proposal]** lines fill the gaps and are the human's call. Trimmed for the agent: the item and draft records that made the file grow (timelines, merged drafts' files) are the shared backlog store's now, so only what is the computer's own is left here. Changes where each project's state lives in [projects.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/projects.md) section 4 and the cleanup in [last-local-state.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/last-local-state.md) section 3.2.

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

## 4. Tests

- `agent/src/store.test.ts`: loading a file with old item and draft records leaves none of them; the saved file has no newline before its first key, and loading it again leaves it as it is.

## 5. Acceptance

- Starting the new agent on Retaskable's own project logs the trim, and `state.json` drops from about 8 MB to under 1.5 MB.
- A week later the file hasn't grown beyond the computer's own state.
- `npm test` passes.
