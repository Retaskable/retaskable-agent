//! Ideas (docs/specs/agent/09.1-ideas.md; Retaskable.com's `agent/src/ideas.ts`
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
//! (`starterAudiences`, shared/audiences.ts), used when the store has no audiences. The `context`
//! and user types are built here too: [`round_context`] (`context()`) from the live titles and the
//! store's rows, [`gather_live`] for the live titles (done items, newest PR merge first) and
//! [`profiles_for`] for the store's audiences (starter set when it has none).
//!
//! [`accept`] carries out Build this (`idea_accept`, [`crate::commands`]): it queues the idea as a spec draft in the shared
//! backlog and answers it `accept` there. Not now and Never are the site's own (`idea_answer`).
//!
//! Still not wired to anything: there's no daily chore that calls [`run_round`] after a merge (needs
//! the orchestrator loop's locks, so two computers don't run a round at once), and nothing calls the context builders
//! yet but tests ([`build_prompt`] still takes `context` already built).
//!
//! Split by responsibility, each piece with its own tests: `model` holds the plain data types and
//! [`refilled`]'s fold, `profiles` is the user types and the prompt builder, `store` reads and writes
//! the shared backlog's own copy of the queue, `write` runs the session and picks the round, and `accept` writes an idea up as a draft.
//! `test_support` is shared fixtures for this module's own tests.
mod accept;
mod context;
mod model;
mod profiles;
mod store;
#[cfg(test)]
mod test_support;
mod write;

pub use accept::accept;
pub use context::{LIVE_MAX, gather_live, live_titles, profiles_for, round_context};
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
