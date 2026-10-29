//! Ideas (docs/specs/agent/09-web-ui.md section 9.3, "Ideas"; Retaskable.com's `agent/src/ideas.ts`
//! and `shared/storeIdeas.ts` are the ground truth). After a slice merges, a read-only `writer`
//! session suggests a few new features or UI improvements, from what went live, the questions asked,
//! and the owner's past answers. [`refilled`] folds its answer into the queue that waits for a person
//! to accept, set aside or turn down: at most [`QUEUE`] ideas wait at a time, and one left unanswered
//! through [`IDEAS_MAX_WAITS_DEFAULT`] refills drops off.
//!
//! [`run_round`] is the session half (`generate`/`write`, agent/src/ideas.ts): it builds the read-only,
//! ephemeral `writer` session (`ideas_result_schema()`, `build_prompt(...)`, the caller's
//! `budget.scout_usd`, role `writer`, `itemId` `_ideas`), runs it through the caller's
//! [`crate::engines::runner::run_session`], and folds the answer into the queue with [`refilled`],
//! leaving the queue waiting as it was when the session fails. [`Ideas`] reads and writes the shared
//! backlog's own copy of that queue (`ideas_list`/`ideas_round`/`idea_answer`); [`queue_from_store`]
//! turns its answer into [`Queue`]. [`starter_profiles`] is a fixed, starter set of user types
//! (`starterAudiences`, shared/audiences.ts), until the real audiences book — one a person can add to
//! and edit — reaches this agent.
//!
//! Still not wired to anything: there's no daily chore that calls [`run_round`] after a merge (needs
//! the orchestrator loop's locks, so two computers don't run a round at once), no command that calls
//! [`Ideas::answer`] for Accept/Not now/Never, and the prompt's `context` (what went live, what was
//! asked, past answers) still has to be built from real store data by the caller: this agent doesn't
//! yet track merged items' dates or asked questions, so [`build_prompt`] keeps taking `context`
//! already built. Accept's draft-writing needs the spec-drafts item; person-added ideas
//! (shared-ideas.md) need their own queue (a `StoreIdea` with `addedBy` set), not ported here.
//!
//! Split by responsibility, each piece with its own tests: `model` holds the plain data types and
//! [`refilled`]'s fold, `profiles` is the user types and the prompt builder, `store` reads and writes
//! the shared backlog's own copy of the queue, and `write` runs the session and picks the round.
//! `test_support` is shared fixtures for this module's own tests.
mod model;
mod profiles;
mod store;
#[cfg(test)]
mod test_support;
mod write;

pub use model::{
    IDEA_EXPERIENCE_MAX, IDEA_FEATURE_MAX, IDEA_FOR_MAX, IDEA_NEW_FEATURE_MAX, IDEA_TITLE_MAX, IDEA_WHY_MAX,
    IDEA_WHY_NOW_MAX, IDEAS_MAX_WAITS_DEFAULT, Idea, IdeaSuggestion, IdeasResult, QUEUE, Queue, ideas_result_schema,
    refilled,
};
pub use profiles::{Profile, build_prompt, starter_profiles};
pub use store::{
    IdeaAnswer, IdeaState, Ideas, IdeasRoundInfo, StoreIdea, StoreIdeaAnswer, StoreIdeaBody, StoreIdeas,
    queue_from_store,
};
pub use write::{Round, Writer, run_round, session_spec};
