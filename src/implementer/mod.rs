//! The implementer session (docs/specs/agent/06.2-implementer.md): the Claude/Cursor session that
//! writes one slice of a backlog item's remaining work and checks it before the agent pushes.
//! Retaskable.com's `agent/src/implementer.ts` (and `agent/src/startFiles.ts`) is the ground truth;
//! this agent ports it one piece at a time.
//!
//! [`start_files`] builds the "Where to start" prompt section (section 6.2's rationale: a median 24
//! reads and searches, 3.7 minutes, before a session's first edit): the item's own spec files, the
//! files its earlier slices changed, and the code its specs name, capped at 15 files in all, plus
//! the commits on that code since each spec last changed. Nothing calls it yet; the prompt
//! template, the result schema, and the session that runs with them are later slices.
pub mod start_files;

pub use start_files::{
    MAX_CHANGED_COMMITS, MAX_FILES, SpecChanges, StartFiles, build, item_spec_files, named_code_paths, render,
};
