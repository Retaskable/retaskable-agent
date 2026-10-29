//! The implementer session (docs/specs/agent/06.2-implementer.md): the Claude/Cursor session that
//! writes one slice of a backlog item's remaining work and checks it before the agent pushes.
//! Retaskable.com's `agent/src/implementer.ts` (and `agent/src/startFiles.ts`) is the ground truth;
//! this agent ports it one piece at a time.
//!
//! [`start_files`] builds the "Where to start" prompt section (section 6.2's rationale: a median 24
//! reads and searches, 3.7 minutes, before a session's first edit): the item's own spec files, the
//! files its earlier slices changed, and the code its specs name, capped at 15 files in all, plus
//! the commits on that code since each spec last changed. [`model`] is the session's structured
//! answer ([`ImplementerResult`]), [`prompt`] fills `prompts/implementer.md` from an item and a
//! project's config, and [`session`] builds the `SessionSpec` either piece runs with and parses the
//! result back. Nothing calls any of it yet: the code gates it should check before a push, a
//! context-size-driven fresh restart, max-turns escalation, and the orchestrator-level handling of
//! `nothing_left`/`person_steps`/`waits_on` all come with the orchestrator loop and the item's own
//! session-tracking store fields.
pub mod model;
pub mod prompt;
pub mod session;
pub mod start_files;
#[cfg(test)]
mod test_support;

pub use model::{ImplementerResult, Playtest, Question, implementer_result_schema};
pub use prompt::{Prompt, build_prompt, checks_text, gate_rule};
pub use session::{Session, Start, new_session_id, run_implementer, session_spec};
pub use start_files::{
    MAX_CHANGED_COMMITS, MAX_FILES, SpecChanges, StartFiles, build, item_spec_files, named_code_paths, render,
};
