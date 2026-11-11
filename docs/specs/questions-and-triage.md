# Questions with no options, triage tasks and bugs typed as ideas — Spec

Status: v0.1, draft (2026-11-11). Backlog: `questions-and-triage`. Copies the TypeScript agent's `optionsFill.ts` (#3316, #3388), `triageTask.ts`, `classify.ts`, `ideaBugSort.ts` and #3305 (https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs: simple-flow.md sections 3.2 and 3.5, jobs-on-server section 6.7, chat-classifier.md, the Bugs spec section 5, chat-reference-buttons.md section 7). Builds on [agent/06.9-triage-claude-looks-at-needs-you-first.md](agent/06.9-triage-claude-looks-at-needs-you-first.md) and [agent/09.3.1-ideas.md](agent/09.3.1-ideas.md).

Theme: Orchestration

Kind: new

## 1. What it is

Four read-only jobs the computer does on a model, each deciding nothing the store decides: giving options to a question that has none, running a triage look when the store sends a `triage` task, sorting a message with the classifier, and moving a bug someone typed as an idea to Bugs. It also names the item on the per-item chat messages.

## 2. Decisions

- **[Decided]** **Options fill.** Each tick, an item or a draft in `needs_clarification` with a question that has fewer than 2 options gets one read-only session, run as the triager is (middle tier, `budget.reviewerUsd`, the engine's own sandbox, prompt `options-fill.md`). It returns one entry per question, 2 to 4 options each, the recommended first. Questions that already had options keep them.
- **[Decided]** One options session at a time per project; none while Claude is signed out. A failed session (no result, or one that doesn't fit) is counted in `optionsTries`; the next try waits 10 minutes; a usage limit counts nothing and waits for the limit to lift. After 2 failures the fixed choices **Go with what Claude would pick** and **Skip this** are saved.
- **[Decided]** The record is marked while its session runs (`running: "options"` on an item, `optionsRunning` on a draft), so only that card reads Being evaluated and the others Waiting to be evaluated. The mark never outlives a restart.
- **[Decided]** **Triage task.** The store sends an item parked for help (what it knows of it); the computer runs the first look (middle tier) or the check of a clear suggestion (top tier), read-only, and answers the structured result. The whether, the second look, the limits, the retries and what the item becomes are the store's. A Retaskable bug the session names is still sent from here, scrubbed, since the computer holds the consent and the known bugs. A failed session answers `ok: false` with a note, `noRetry` for a budget stop, and the limit's end time for a usage limit.
- **[Decided]** **Classify.** One structured call on Haiku sorts a message into `bug`, `idea`, `question` or `other` with a confidence; a bug or idea at 0.8 or above counts as confident. Not called when Claude is signed out or the day's ask budget is spent; any failure or a 20-second timeout is "unavailable", never an error.
- **[Decided]** **Bug typed as an idea.** When this computer first claims the plan queued for a person's idea (not a hand-off, move, answer or send-back; the idea has no bug mark and no pictures), the idea's text is classified once. A confident bug marks the idea a bug and the plan's request is rewritten as a fix. Anything else, or any error, leaves it an idea; it never blocks the draft.
- **[Decided]** **Chat messages name the item.** A Retaskable chat message about one item (`building:`, `needs:`, `merged:`, `built:`) carries that item's id as `itemId`; the draft-level ones carry none.
- **[Decided]** The Rust names: `src/triage/options_fill.rs` (what to ask, how to read the answer, and `next`: leave, wait for the limit, retry or save), `src/triage/task.rs` (`run` answers `{ triage }` or `{ check }`, or a failure with `noRetry` and the limit), `src/ask/classify.rs`, `src/ideas/bug_sort.rs` (`sort`, with the pure `should_sort` and `first_claim`; `idea_mark_bug` is `Ideas::mark_bug`). Each takes the session as a closure over `run_session`, so the caller picks the engine, tier and place.

## 3. Behavior, in build order

1. **Classify** (`classify`, pure parts tested; the session through the engine layer).
2. **Options fill**, with the marks, tries and fixed choices of section 2; saved on the item's record (reported to the store with its details) or the draft's.
3. **Triage task**: a `triage` job handler that runs the session and answers `{ triage }` or `{ check }`; the prompts are `prompts/triage.md` and the check's.
4. **Bug typed as an idea**, called when a draft is claimed from the store's queue; `should_sort` is pure.
5. **`itemId`** on the chat messages that are about one item.
6. **Tests:** options saved for the questions that lacked them, fixed choices after 2 failures, a limit not counted, answered-meanwhile left alone; the triage task's answers and failures; classify's confidence and unavailable cases; a confident bug moved, an idea, a picture-bearing idea and a re-claim left alone; `itemId` present and absent. Ends with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## 4. Acceptance

- No question reaches a person with fewer than 2 options; nothing waits for ever.
- Only the question whose session runs reads Being evaluated.
- A triage task never changes an item itself; it only answers.
- A bug typed as an idea moves to Bugs once, on a confident read; a failed classifier leaves it.
- An item's chat messages carry its id.
