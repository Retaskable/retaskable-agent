# Sorting what the plus box sent — Spec

Status: v0.1, draft (2026-11-19). Copies the TypeScript agent's `sortIdea.ts` and `ideaBugSort.ts` (Retaskable.com #3964, `docs/specs/add-and-design-review.md` sections 4 and 5, deleted with that agent in #4161) and replaces the classifier call of [questions-and-triage.md](questions-and-triage.md) "Bug typed as an idea".

Theme: Orchestration

Kind: new

## 1. What it is

One box takes anything, and this computer sorts it afterwards. When it first claims the plan queued for a person's idea, the idea's words are sorted once into `idea`, `bug` or `unsure`, with a plain reason, a clean label, whether people will see the change (`visible`) and, for a message that holds several things, a proposed `split`. The sort is written to the store's own copy of the idea with `idea_sort`. The store decides everything that follows (a bug moves to Bugs and its queued plan is rewritten as a fix; a plan waits for a design review only when the sort says `visible` and the plan's tier is not routine); the agent sends facts and never a threshold.

## 2. Decisions

- **[Decided]** `idea_sort { id, kind, by, reason?, title?, visible?, split? }` (agents only) replaces `idea_classify` for a person's idea. `kind` is `idea`, `bug`, `unsure` or `later`; `by` is `rule` or `agent`. The store answers the idea. It refuses (`conflict`) an idea a person already sorted, planned or answered; that is logged and the draft carries on. `idea_classify` stays only for the chat's own classifier (`ask/classify.rs`), never for ideas.
- **[Decided]** **The cheap check first** (`ideas::sort_idea::rule_sort`, plain code, `preSort`). It settles only the obvious, in one thing: broken words (crash, doesn't work, not working, broken, won't open, can't save, nothing happens, error, bug, wrong...) with no wish word is `bug`, "it says something is broken"; wish words (add, what if, I wish, could we, can we, let me, allow, support, new...) with no broken word is `idea`, "it asks for something new". Both, neither, a list, "also", "another thing", "plus" or several sentences go to the agent. A rule sort has a label (the first line, cut at a word to 80 characters) and **no `visible`**: the plan's own test decides.
- **[Decided]** **Then one read-only session**, the design board's kind (scout tier, scratch directory, no shell), with `prompts/sort-idea.md` and the closed structure `{ kind: idea | bug | unsure, reason, title, visible, split? }`. The answer is checked again in plain code (`read_answer`): a `reason` and `title` of one line (120 and 80 characters), `split` of 2 to 4 one-line pieces of at most 400 characters, no other keys. An answer that fails its check is no answer.
- **[Decided]** **No answer** (signed out, no Claude here, the day's budget spent, the session or its check fails) writes `later`: an idea marked "Sorting later", never a guess at a bug. A person's words are never changed: `title` is only a label.
- **[Decided]** An idea with a picture is read by its words alone, which may say "see the picture": it is `unsure`, by rule, reason "it has a picture to look at", and a person says.
- **[Decided]** **When.** Right after a first claim (not a hand-off, move, answer or send-back; not a resumed draft; not while Claude is signed out) of a waiting person's idea that has no bug mark and no sort yet (or only `later`). When the store answers `bug`, the claimed copy's request is rewritten as a fix and its kinds are `fix`, as the store has done to its own copy. Never blocks or fails the draft.
- **[Decided]** **Sorting later, retried.** When a drafting pass finds nothing to claim, this computer reads the store's ideas, at most once every ten minutes for a project, and sorts each waiting idea with `later`, no bug mark and no picture, at most three at a time and a given idea no oftener than every ten minutes. Only a real answer (`kind` not `later`) is written. This is a read the agent does with what it already holds; no new schedule, no queue.
- **[Decided]** **Design review only when visible.** The agent adds nothing to its plan facts for this: the store reads the idea's `sort.visible` itself (`needsDesignFrom`), so a plan whose idea was sorted `visible: false`, whose words are only wording (typo, rename, "change the text of...") or whose tier is routine for the project's risk profile reports `needsDesign: false` and goes on as planned. A plan with no sort yet is left to the plan's own test. The drafter's own `needs_design_of` still gates the facts it sends (`first`, `preview`, `itemKinds`).
- **[Decided]** **The design the person picked.** "Looks good" sends the plan back with `The design is approved. Plan it as shown: "<variation name>"...`. When this computer claims a send-back whose notes say so and the plan's stored `designReview` names one of its variations, it keeps `chosen` (the variation's index) in that review (`design_board::review::with_chosen`, written through `draft_details_report`) with the timeline line "Design chosen: <name>".
- **[Decided]** The Rust names: `src/ideas/sort_idea.rs` (the answer, the rule, the schema, the label), `src/ideas/bug_sort.rs` (`sort`, `sort_later`), `Ideas::sort` (`idea_sort`), the draft host's `sort_idea` session, `prompts/sort-idea.md`.

## 3. Tests

The rule settles broken and wish words and refuses both, none, a list and several sentences; the answer's check refuses a multi-line reason, a split of one, an unknown key and a missing field; a bug the store moves rewrites the claimed request; an idea, an unsure one, a picture-bearing one (sent as `unsure` by rule) and a `later` one are sent as such with no rewrite; a failed session writes `later`; a person's sort (`conflict`) leaves the draft alone; the retry sorts at most three `later` ideas, skips one tried in the last ten minutes and writes nothing for a `later` answer; `chosen` is kept for an approved variation and not for other notes.

## 4. Acceptance

- A bug typed in the plus box moves to Bugs once; a clear wish stays an idea with no session at all; an unsure one waits for a person; an offline computer leaves "Sorting later" and sorts it when it can.
- A visible change gets its designs only when the sort said visible and the tier is not routine; a wording-only or behaviour-only idea goes on as planned.
