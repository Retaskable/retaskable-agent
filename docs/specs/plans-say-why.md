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

## 4. Acceptance

- No failed plan is reported with an empty reason.
- A retry's prompt names the reason, or says none was recorded.
- Build this on an idea with a plan under way is refused and queues no draft.
