//! Stage 3 of review: the one Opus session that gives the final yes or no on a slice
//! (docs/specs/agent/06.3-review.md, "Stage 3: final review"). It's told to verify, not
//! re-review: it confirms or throws out each scout finding (`crate::scouts`, stage 2, already
//! built) by reading only the cited lines, and spot-checks what the scouts can't see. Retaskable.com's
//! `agent/src/review.ts` (the reviewer half) is the ground truth.
//!
//! [`model`] holds the session's structured result (`ReviewerResult`), [`prompt`] fills
//! `prompts/reviewer.md` from an item, the stage 1 results and the scouts' findings, and [`run`]
//! builds the session and renders its verdict as the PR comment the orchestrator posts. Nothing
//! calls any of it yet: turning `verdict` into the item's next `item_lifecycle::ReviewOutcome`
//! (including `person_steps`, which isn't one of that enum's variants yet), looking up the User
//! Manual's approved versions, and posting the comment through `gh pr comment` all come with the
//! orchestrator loop, which doesn't wire review at all yet.
mod model;
mod prompt;
mod run;

pub use model::{DismissedFinding, ReviewerComment, ReviewerResult, reviewer_result_schema};
pub use prompt::{ReviewerContext, build_prompt};
pub use run::{Reviewer, format_review, session_spec};
