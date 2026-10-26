//! Release notes (docs/specs/agent/09-web-ui.md section 9.3; Retaskable.com's
//! `agent/src/releaseNotes.ts` is the ground truth). Gathers what merged into the base branch in a
//! date range (`gh pr list` + `git log --first-parent`), groups it by the backlog item that owns
//! each PR, builds the JSON "facts" and the `prompts/release-notes.md` prompt from them, and renders
//! the note's final Markdown from the model's structured answer
//! ([`ReleaseNotesResult`]/[`release_notes_result_schema`]).
//!
//! Not wired to anything yet: there's no command that calls this, no saved/published note state, and
//! no model/tier selection. A caller puts this together with [`crate::session`] itself: build a
//! [`crate::session::SessionSpec`] with `schema: release_notes_result_schema()`,
//! `prompt: build_prompt(...)`, `budget_usd: cfg.budget.release_notes_usd`, and
//! `start: SessionStart::Ephemeral` (the note is read-only and interactive like an ask — there's
//! nothing to resume), run it with [`crate::session::run_session`], then render the result with
//! [`markdown`]. That wiring, and the Draft-based grouping, chore/fix split and spec-link resolution
//! the TypeScript agent layers on top of this, are later slices.
//!
//! Split by responsibility, each piece with its own tests: `model` holds the plain data types,
//! `group` turns gathered PRs/commits into sections and the JSON facts, `render` turns a grouping
//! (and the model's optional answer) into Markdown, and `gather` runs `gh`/`git` to collect what
//! merged. `test_support` is shared fixtures for this module's own tests.
mod gather;
mod group;
mod model;
mod render;
#[cfg(test)]
mod test_support;

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
