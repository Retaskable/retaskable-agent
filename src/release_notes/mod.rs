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
//! caller's [`crate::engines::runner::run_session`], falling back to the plain list when it fails. `saved` reads the
//! notes the store keeps (`releases_get`), offering it the repo's `docs/releases` files once first. Nothing calls
//! any of it yet: the command that asks for a note, the tier and whether the local model is free come with the
//! orchestrator loop and the cloud link's commands. The playtest-fix split waits for the agent's own chores.
//!
//! Split by responsibility, each piece with its own tests: `model` holds the plain data types,
//! `group` turns gathered PRs/commits into sections and the JSON facts, `render` turns a grouping
//! (and the model's optional answer) into Markdown, `gather` runs `gh`/`git` to collect what
//! merged, `write` runs the session and picks the note, and `saved` reads the store's saved notes. `test_support` is shared fixtures for this module's own tests.
mod gather;
mod group;
mod model;
mod render;
mod saved;
#[cfg(test)]
mod test_support;
mod write;

pub use gather::{
    DEFAULT_MAX_PRS, GatherArgs, commit_log_args, fill_draft_files, gather, gather_commits, gather_merged_prs,
    merged_pr_args, read_base_files,
};
pub use group::{DEFAULT_FULL_ENTRIES, GroupInput, counts, facts, for_users, group, pr_owner, spec_links, spec_title};
pub use model::{
    Commit, Counts, Entry, Highlight, ItemRef, MergedPr, NoteEntry, PrRef, Range, ReleaseNotesResult, Section,
    SpecLink, release_notes_result_schema,
};
pub use render::{MAX_HIGHLIGHTS, build_prompt, empty_note, front_matter_lines, local_day, markdown, note_markdown};
pub use saved::{
    RELEASE_BODY_MAX_BYTES, RELEASE_TITLE_MAX, RELEASES_DIR, ReleaseFile, ReleaseOffer, SavedReleases, StoreRelease,
    offered_file, read_release_files, release_of_file,
};
pub use write::{Writer, Written, check_range, session_spec, write_note, writer_model};
