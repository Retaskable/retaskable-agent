# Plans that fail, are dropped or are already under way — Spec

Status: v0.2, draft (2026-11-11). Backlog: `plans-say-why`. Copies the TypeScript changes #3314, #3290 and #3250 (https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs: two-steps.md section 15, simple-flow.md section 6, drafting-table.md, team-state-on-server/03-behavior.md, shared-ideas/03.3-the-agent-slice-3.md). Builds on [agent/06.5-spec-drafts.md](agent/06.5-spec-drafts.md) and [agent/09.3.1-ideas.md](agent/09.3.1-ideas.md).

Theme: Orchestration

Kind: new

## 1. What it is

Three small rules about a plan (a spec draft) that the TypeScript agent has and the Rust agent lacks: a failed plan always records why and Try again hands that reason to the next drafter; a plan dropped on this computer reaches the store, so no one is offered Approve on it; and Build this on an idea whose plan is already under way is refused with a word, not turned into a second draft.

## 2. Decisions

- **[Decided]** Plans live in the store, not on this computer: the agent keeps no local plan record or timeline. A failure is reported to the store as the draft's `summary` (`orchestrator/driver/drafts.rs`, `Ctx::failed`), and every failure goes through it. A blank reason becomes a plain fallback built from what is known (never asked of a model).
- **[Decided]** A draft claimed again after its first failure (`retried`) starts a new drafter session from the request, carrying on from the kept branch. Its prompt (`src/drafts/failure.rs`, `retry_text`) gives the recorded reason, or says none was recorded, and tells the drafter to return `failed` with an exact reason if it still cannot write the plan.
- **[Decided]** A plan dropped here is never kept by this computer, so the store is the only place it can be offered Approve; there is nothing to report over.
- **[Decided]** Build this on an idea that already has a `planId` is refused with "That idea is already being planned: its plan is in Plans." and makes no draft.

## 3. Behavior

1. **A failed plan says why.** `reason_or` trims a reason and uses a fallback when blank: "The plan failed and no reason was recorded."; a drafter that errors with no message says "Writing the plan stopped with an error that had no message (<kind>)."; a `failed` result with no summary says "The drafter stopped without giving a reason."; a publish failure with no words says "no reason given". The first failure goes back to the queue as `Trying again by itself: <reason>`; a second waits for a person as `failed`, with the reason as its summary.
2. **Try again uses it.** A claimed draft with `retried` set gets `retry_text(failure_reason(draft))` after the drafter prompt. `failure_reason` is the draft's `error`, else its `summary` after the `Trying again by itself: ` prefix, else none.
3. **An idea under way.** `accept` (`src/ideas/accept.rs`) checks the idea's `planId` first and refuses before any draft is queued.
4. **Tests:** `reason_or`/`error_reason`, `failure_reason`, `retry_text` with and without a reason (`src/drafts/failure.rs`); Build this refused with no draft queued (`src/ideas/accept.rs`).

## 3.1 Details the rules settle

What the code does today, then what is still open. Everything below that is not a description of the code is a **[Proposal]**.

**As built**

- **Failure, step by step.** `Ctx::failed` first runs the report's summary through `reason_or(.., NO_REASON)`. If the draft was claimed with `retried` true (the store's `retried` field is exactly `true`), it reports `needs_person` with detail `failed` and the reason as the summary. Otherwise it reports status `queued`, summary `Trying again by itself: <reason>` and `retried: true`, once. So a draft is tried at most twice by itself, never a third time.
- **Every path ends in `failed`.** A drafter that returns an error, a join error, a `failed` result, a publish failure and a failure to read the backlog's items (`couldn't read the backlog's items: <error>`) all go through `Ctx::failed`; none reports `failed` any other way.
- **What the retry's drafter is given.** `retry_text` collapses the reason's whitespace to single spaces and clips it to 300 characters (`collapse_and_clip`, an ellipsis marks a cut), so a long log in a reason is one line. The text is appended after the drafter prompt, never replaces it; the request is the same as the first try's.
- **Finding the reason.** `failure_reason` prefers the draft's `error` over its `summary`. A blank `error` or `summary` is skipped. A summary that does not start with `Trying again by itself: ` (a plan that was written, for example) gives none, and the retry text says no reason was recorded.
- **Build this's order of checks.** `accept` lists the store's ideas and refuses, in this order, with no draft queued: the idea is not there ("That idea isn't there any more."); `planId` is non-blank ("That idea is already being planned: its plan is in Plans."); the idea is no longer waiting ("That idea was already answered by @<login>."). Only then does it queue the draft and answer the idea `accept`. A store that queues the draft but cannot take the answer keeps the draft and logs a warning, since the person did ask for it.
- **A blank `planId`** (empty or spaces) counts as no plan.

**Open edges**

- **A reason is as long as the drafter made it.** The store gets the whole reason as the summary; only the retry prompt clips it. **[Proposal]** Leave it so: the site decides how much of a summary to show.
- **A second failure of a retry that was started by a person** (Try again after `failed`) goes through the same path: `retried` is already true, so it waits as `failed` again at once, with the new reason, and does not queue itself. **[Proposal]** This is right: a person asked and sees the new reason; no loop.
- **Two Build this presses at once** (two computers, or a double press) are settled by the store's answer, not by the agent: the second press finds the idea answered and gets "already answered". **[Proposal]** The agent adds no lock of its own, because the store is the one place that decides.
- **A plan dropped while its drafter runs** is not noticed by this computer's `failed`: if the drafter then fails, the report goes to a draft the store has dropped, and the store's refusal is logged, not shown. **[Proposal]** Nothing more is needed; no one is offered Approve on a dropped plan.
- **What a person sees.** The plan's card in Plans shows the summary as is: the reason, with the `Trying again by itself:` prefix while a retry is queued. The refusal words are shown where the person pressed Build this. No new text is needed.

## 4. Acceptance

- No failed plan is reported with an empty reason.
- A retry's prompt names the reason, or says none was recorded.
- Build this on an idea with a plan under way is refused and queues no draft.
- A plan that fails twice is tried no third time by itself and waits as `failed` with the second reason.
- A retry's prompt carries a reason of at most 300 characters on one line.
- A blank `planId` does not refuse Build this.
