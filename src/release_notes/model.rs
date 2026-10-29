//! The plain data this module passes around: what was gathered ([`MergedPr`], [`Commit`]), how it's
//! grouped ([`Section`], [`Entry`]), and the model's structured answer ([`ReleaseNotesResult`]).
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backlog::ItemStatus;

/// One PR `gh pr list` reported as merged into the base branch.
#[derive(Debug, Clone)]
pub struct MergedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub body: String,
    pub merged_at: String,
    pub head_ref_name: String,
    pub merge_commit_oid: Option<String>,
}

/// One commit `git log --first-parent` reported on the base branch.
#[derive(Debug, Clone)]
pub struct Commit {
    pub sha: String,
    pub date: String,
    pub author: String,
    pub subject: String,
}

/// The range a note covers: `from`/`to` are ISO instants, `generated_at` is when it was built.
#[derive(Debug, Clone)]
pub struct Range {
    pub from: String,
    pub to: String,
    pub generated_at: String,
}

#[derive(Debug, Clone)]
pub struct PrRef {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ItemRef {
    pub id: String,
    pub title: String,
    pub status: ItemStatus,
}

/// One thing in a section: a backlog item (one or more merged PRs), or, when no item claims a PR or
/// commit, the PR/commit standing for itself under `fallback`.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub item: Option<ItemRef>,
    pub fallback: String,
    pub prs: Vec<PrRef>,
    pub commit: Option<Commit>,
    pub specs: Vec<String>,
    pub chore: bool,
}

/// A group of entries: one per spec an item names (`spec:<path>`), one for chores, and one
/// (`other`) for everything with no spec, no owning item, or no item at all.
#[derive(Debug, Clone)]
pub struct Section {
    pub key: String,
    pub heading: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub prs: usize,
    pub items: usize,
    pub chores: usize,
}

/// `ReleaseNotesResult` (agent/src/schemas.ts): the structured answer a release-notes session
/// returns.
#[derive(Debug, Clone, Deserialize)]
pub struct Highlight {
    pub key: String,
    pub headline: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NoteEntry {
    pub key: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseNotesResult {
    pub status: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub highlights: Vec<Highlight>,
    #[serde(default)]
    pub entries: Vec<NoteEntry>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `RELEASE_NOTES_RESULT_SCHEMA` (agent/src/schemas.ts), for `SessionSpec.schema`.
pub fn release_notes_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": { "enum": ["ok", "failed"] },
            "title": { "type": "string" },
            "summary": { "type": "string" },
            "highlights": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "headline": { "type": "string" },
                        "text": { "type": "string" }
                    },
                    "required": ["key", "headline", "text"]
                }
            },
            "entries": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "text": { "type": "string" }
                    },
                    "required": ["key", "text"]
                }
            },
            "error": { "type": "string" }
        },
        "required": ["status", "title", "summary", "highlights", "entries"]
    })
}
