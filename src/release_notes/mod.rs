//! Release notes (docs/specs/agent/09-web-ui.md section 9.3; Retaskable.com's
//! `agent/src/releaseNotes.ts` is the ground truth). Gathers what merged into the base branch in a
//! date range (`gh pr list` + `git log --first-parent`), groups it by the backlog item that owns
//! each PR, builds the JSON "facts" and the `prompts/release-notes.md` prompt from them, and renders
//! the note's final Markdown from the model's structured answer
//! ([`ReleaseNotesResult`]/[`release_notes_result_schema`]).
//!
//! [`write_note`] is the session half of the TypeScript agent's `generate`/`write`: after [`check_range`] and
//! [`gather`]/[`group`], it builds the read-only, ephemeral `writer` session (`release_notes_result_schema()`,
//! `build_prompt(...)`, `budget.release_notes_usd`, [`writer_model`] of the scouts' tier) and runs it through the
//! caller's [`crate::engines::runner::run_session`], falling back to the plain list when it fails. Nothing calls it
//! yet: the command that asks for a note, the tier and whether the local model is free, and the store's
//! `releases_get`/`release_set` that keep the note come with the orchestrator loop and the cloud link's commands.
//! The Draft-based grouping, chore/playtest-fix split and spec-link resolution the TypeScript agent layers on top
//! are also still to port.
//!
//! Split by responsibility, each piece with its own tests: `model` holds the plain data types,
//! `group` turns gathered PRs/commits into sections and the JSON facts, `render` turns a grouping
//! (and the model's optional answer) into Markdown, `gather` runs `gh`/`git` to collect what
//! merged, and `write` runs the session and picks the note. `test_support` is shared fixtures for this module's own tests.
mod gather;
mod group;
mod model;
mod render;
#[cfg(test)]
mod test_support;
mod write;

pub use gather::{
    DEFAULT_MAX_PRS, GatherArgs, commit_log_args, gather, gather_commits, gather_merged_prs, merged_pr_args,
    read_base_files,
};
pub use group::{DEFAULT_FULL_ENTRIES, clip, counts, facts, for_users, group, pr_owner, spec_title};
pub use model::{
    Commit, Counts, Entry, Highlight, ItemRef, MergedPr, NoteEntry, PrRef, Range, ReleaseNotesResult, Section,
    release_notes_result_schema,
};
pub use render::{
    MAX_HIGHLIGHTS, ProjectWords, build_prompt, empty_note, front_matter_lines, local_day, markdown, note_markdown,
    project_words, render_prompt,
};
pub use write::{Writer, Written, check_range, session_spec, write_note, writer_model};
