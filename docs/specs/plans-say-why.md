# Plans that fail, are dropped or are already under way — Spec

Status: v0.1, draft (2026-11-11). Backlog: `plans-say-why`. Copies the TypeScript changes #3314, #3290 and #3250 (https://github.com/Retaskable/Retaskable.com/tree/main/docs/specs: two-steps.md section 15, simple-flow.md section 6, drafting-table.md, team-state-on-server/03-behavior.md, shared-ideas/03.3-the-agent-slice-3.md). Builds on [agent/06.5-spec-drafts.md](agent/06.5-spec-drafts.md) and [agent/09.3.1-ideas.md](agent/09.3.1-ideas.md).

Theme: Orchestration

Kind: new

## 1. What it is

Three small rules about a plan (a spec draft) that the TypeScript agent has and the Rust agent lacks: a failed plan always records why and Try again hands that reason to the next drafter; a plan dropped on this computer reaches the store, so no one is offered Approve on it; and Build this on an idea whose plan is already under way is refused with a word, not turned into a second draft.

## 2. Decisions

- **[Decided]** Every failure of a draft goes through one function (`fail_draft`, `src/drafts/`). A blank reason becomes a plain fallback the caller builds from what is known (never asked of a model). The timeline line reads `Failed: <reason>` (first 300 characters, whitespace squeezed) unless the caller gives its own line.
- **[Decided]** Try again on a failed plan starts a new drafter session from the request, carrying on from the kept branch. Its prompt gives the recorded reason; when none was recorded it says so and lists the plan's last 5 timeline lines (each cut to 200), and tells the drafter to return `failed` with an exact reason if it still cannot write the plan.
- **[Decided]** A plan discarded or merged on this computer while the store still lists its row as waiting for approval is reported over to the store. Approve on it answers "This plan was dropped, so there's nothing to approve." (merged: "This plan is already in.").
- **[Decided]** Build this on an idea that already has a `planId` is refused with 409 "That idea is already being planned: its plan is in Plans." and makes no draft.
- **[Proposal]** The Rust names: `fail_draft`, `failure_reason` in `src/drafts/`; the report-over in `src/drafts/actions.rs`; the idea check in `src/ideas/accept.rs`.

## 3. Behavior, in build order

1. **A failed plan says why.** Route every `state: failed` write in `src/drafts/` through `fail_draft(id, reason, patch, line?)`: the reason is trimmed, a blank one becomes "The plan failed and no reason was recorded."; a drafter that fails with no message says "Writing the plan stopped with an error that had no message (<kind>)."; a `failed` result with no summary says "The drafter stopped without giving a reason."; a push or pull-request failure with no words says "no reason given". `failure_reason(draft)` is the draft's `error`, else its newest timeline line that starts with `Failed: ` with that prefix removed, else none.
2. **Try again uses it.** The retry prompt (`src/drafts/prompt.rs`) is built from `failure_reason` and the timeline as in section 2. Everything else about Try again (no session id, counters cleared, branch kept, the timeline line "Trying again from the start, asked by @x: …") is as agent/06.5-spec-drafts.md says.
3. **A dropped plan reaches the store.** On the next read of the store's drafts, and at once on an Approve, send back or answer for it, a plan over here (discarded or merged) that is let go or lost and that the store still lists as waiting is taken, its details written and its end reported. A row another computer holds or finished is left alone and the reason logged. An Approve for a plan over here is answered before it is taken, so no hold is left on its row.
4. **A plan that was dropped comes back as a new copy.** A taken-over draft whose old local copy is marked moved away replaces it; any other existing copy is kept.
5. **An idea under way.** `accept` for Build this checks the idea's `planId` first and refuses as in section 2 before any worktree or draft is made.
6. **Tests:** each failure path records a reason (blank, no-message error, `failed` with no summary); `failure_reason` from `error` and from the timeline; the retry prompt with and without a reason; a dropped plan reported to the store, left alone when held elsewhere; Approve on a dropped and on a merged plan; Build this refused with no draft made. Ends with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.

## 4. Acceptance

- No failed plan has an empty reason, in the record or on its timeline.
- Try again's prompt names the reason, or says none was recorded and lists the last timeline lines.
- A plan dropped here is never offered Approve anywhere, and Approve on it answers in the words above.
- Build this on an idea with a plan under way answers 409 and leaves no new draft.
