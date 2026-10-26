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
use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;

use crate::backlog::{Item, ItemStatus};
use crate::config::ProjectType;

/// `MAX_HIGHLIGHTS` (shared/releaseNote.ts).
pub const MAX_HIGHLIGHTS: usize = 5;
/// `releaseMaxPrs`'s default (shared/tuning.ts): more than this many merged PRs in one range is
/// refused rather than guessed at.
pub const DEFAULT_MAX_PRS: u32 = 200;
/// `releaseFullEntries`'s default (shared/tuning.ts): entries beyond this many are trimmed to just
/// their title before they go to the model.
pub const DEFAULT_FULL_ENTRIES: usize = 120;

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

/// The item a merged PR belongs to: first by `pr_url`, else by the longest `retaskable/<id>-` branch
/// prefix match (`releaseNotes.ts`'s owner lookup, simplified: this agent's `Item` doesn't carry the
/// legacy branch prefix or a `mergedPrs` list).
pub fn pr_owner<'a>(pr: &MergedPr, items: &'a [Item]) -> Option<&'a Item> {
    if let Some(item) = items.iter().find(|i| i.pr_url.as_deref() == Some(pr.url.as_str())) {
        return Some(item);
    }
    items
        .iter()
        .filter(|i| pr.head_ref_name.starts_with(&format!("retaskable/{}-", i.id)))
        .max_by_key(|i| i.id.len())
}

fn to_pr_ref(pr: &MergedPr) -> PrRef {
    PrRef {
        title: pr.title.clone(),
        body: pr.body.clone(),
    }
}

fn status_word(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::OnMain | ItemStatus::InProduction => "done",
        _ => "in progress",
    }
}

fn ensure_section(by_key: &mut HashMap<String, Section>, key: &str, heading: &str) {
    by_key.entry(key.to_string()).or_insert_with(|| Section {
        key: key.to_string(),
        heading: heading.to_string(),
        entries: Vec::new(),
    });
}

/// The spec's own title: its first `#` heading with a trailing "— Spec" (any dash, case-insensitive)
/// dropped, else the file's name (`releaseNotes.ts`'s `specTitle`).
pub fn spec_title(path: &str, content: Option<&str>) -> String {
    if let Some(content) = content {
        let heading = Regex::new(r"(?m)^#\s+(.+?)\s*$").unwrap();
        if let Some(caps) = heading.captures(content) {
            let suffix = RegexBuilder::new(r"\s+[-—–]\s*spec$")
                .case_insensitive(true)
                .build()
                .unwrap();
            let title = suffix.replace(caps[1].trim(), "").trim().to_string();
            if !title.is_empty() {
                return title;
            }
        }
    }
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Groups merged PRs and commits by the backlog item that owns them (`releaseNotes.ts`'s grouping,
/// simplified: no Draft reconciliation, and chores land in one `"chores"` section instead of being
/// split from playtest fixes). `files` is each named spec's content at the base branch (or `None` if
/// it couldn't be read), from [`read_base_files`], used to title that spec's section.
pub fn group(
    prs: &[MergedPr],
    commits: &[Commit],
    items: &[Item],
    files: &HashMap<String, Option<String>>,
) -> Vec<Section> {
    let mut sorted: Vec<&MergedPr> = prs.iter().collect();
    sorted.sort_by(|a, b| a.merged_at.cmp(&b.merged_at));

    let mut by_key: HashMap<String, Section> = HashMap::new();
    let mut merge_commit_oids: HashSet<&str> = HashSet::new();

    for pr in &sorted {
        if let Some(oid) = &pr.merge_commit_oid {
            merge_commit_oids.insert(oid.as_str());
        }

        let owner = pr_owner(pr, items);
        let (section_key, section_heading, entry_key, item_ref, fallback, specs, chore) = match owner {
            Some(item) if item.milestone.as_deref() == Some("chores") => (
                "chores".to_string(),
                "Chores".to_string(),
                format!("item:{}", item.id),
                Some(ItemRef {
                    id: item.id.clone(),
                    title: item.title.clone(),
                    status: item.status,
                }),
                String::new(),
                Vec::new(),
                true,
            ),
            Some(item) => match item.spec_refs.first() {
                Some(spec) => (
                    format!("spec:{spec}"),
                    spec_title(spec, files.get(spec).and_then(|c| c.as_deref())),
                    format!("item:{}", item.id),
                    Some(ItemRef {
                        id: item.id.clone(),
                        title: item.title.clone(),
                        status: item.status,
                    }),
                    String::new(),
                    item.spec_refs.clone(),
                    false,
                ),
                None => (
                    "other".to_string(),
                    "Other changes".to_string(),
                    format!("item:{}", item.id),
                    Some(ItemRef {
                        id: item.id.clone(),
                        title: item.title.clone(),
                        status: item.status,
                    }),
                    String::new(),
                    Vec::new(),
                    false,
                ),
            },
            None => (
                "other".to_string(),
                "Other changes".to_string(),
                format!("pr:{}", pr.number),
                None,
                pr.title.clone(),
                Vec::new(),
                false,
            ),
        };

        ensure_section(&mut by_key, &section_key, &section_heading);
        let section = by_key.get_mut(&section_key).unwrap();
        match section.entries.iter_mut().find(|e| e.key == entry_key) {
            Some(existing) => existing.prs.push(to_pr_ref(pr)),
            None => section.entries.push(Entry {
                key: entry_key,
                item: item_ref,
                fallback,
                prs: vec![to_pr_ref(pr)],
                commit: None,
                specs,
                chore,
            }),
        }
    }

    for commit in commits {
        if merge_commit_oids.contains(commit.sha.as_str()) {
            continue;
        }
        let key = format!("commit:{}", &commit.sha[..commit.sha.len().min(7)]);
        ensure_section(&mut by_key, "other", "Other changes");
        let section = by_key.get_mut("other").unwrap();
        if !section.entries.iter().any(|e| e.key == key) {
            section.entries.push(Entry {
                key,
                item: None,
                fallback: commit.subject.clone(),
                prs: Vec::new(),
                commit: Some(commit.clone()),
                specs: Vec::new(),
                chore: false,
            });
        }
    }

    let mut spec_keys: Vec<String> = by_key.keys().filter(|k| k.starts_with("spec:")).cloned().collect();
    spec_keys.sort();

    let mut ordered = Vec::new();
    for key in &spec_keys {
        if let Some(section) = by_key.remove(key) {
            ordered.push(section);
        }
    }
    if let Some(section) = by_key.remove("chores") {
        ordered.push(section);
    }
    if let Some(section) = by_key.remove("other") {
        ordered.push(section);
    }
    ordered.into_iter().filter(|s| !s.entries.is_empty()).collect()
}

/// How many PRs, distinct items and chores a grouping covers.
pub fn counts(sections: &[Section], total_prs: usize) -> Counts {
    let mut items = HashSet::new();
    let mut chores = 0usize;
    for section in sections {
        for entry in &section.entries {
            if let Some(item) = &entry.item {
                items.insert(item.id.clone());
            }
            if entry.chore {
                chores += 1;
            }
        }
    }
    Counts {
        prs: total_prs,
        items: items.len(),
        chores,
    }
}

/// The sections a release note shows people: everything but chores.
pub fn for_users(sections: &[Section]) -> Vec<&Section> {
    sections.iter().filter(|s| s.key != "chores").collect()
}

fn title_of(entry: &Entry) -> String {
    match &entry.item {
        Some(item) => format!("{} {}", item.id, item.title),
        None => entry.fallback.clone(),
    }
}

/// `cut()` (shared/text.ts): truncates to `n` characters (dropping the last if it would split a
/// pair), plus `"…"`. Rust `char`s are already whole Unicode scalar values, so this is a plain
/// char-count truncation — there is no surrogate pair to protect against.
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let kept: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The JSON "facts" the prompt gives the model: every user-facing section, entries beyond
/// `max_full_entries` trimmed to just their title (smallest, most "other"-like sections first, so a
/// handful of busy spec sections keep their detail).
pub fn facts(sections: &[Section], max_full_entries: usize) -> Value {
    let visible = for_users(sections);
    let total: usize = visible.iter().map(|s| s.entries.len()).sum();
    let mut over = total.saturating_sub(max_full_entries);

    let mut by_size: Vec<usize> = (0..visible.len()).collect();
    by_size.sort_by_key(|&i| {
        let section = visible[i];
        let rank = if section.key == "other" { 0 } else { 1 };
        (rank, section.entries.len())
    });

    let mut brief: HashSet<usize> = HashSet::new();
    for i in by_size {
        if over == 0 {
            break;
        }
        brief.insert(i);
        over = over.saturating_sub(visible[i].entries.len());
    }

    let sections_json: Vec<Value> = visible
        .iter()
        .enumerate()
        .map(|(i, section)| {
            let entries_json: Vec<Value> = section
                .entries
                .iter()
                .map(|entry| {
                    if brief.contains(&i) {
                        json!({ "key": entry.key, "title": title_of(entry), "titleOnly": true })
                    } else {
                        let mut obj = serde_json::Map::new();
                        obj.insert("key".to_string(), json!(entry.key));
                        match &entry.item {
                            Some(item) => {
                                obj.insert("item".to_string(), json!(item.title));
                                obj.insert("status".to_string(), json!(status_word(item.status)));
                            }
                            None => {
                                obj.insert("title".to_string(), json!(entry.fallback));
                            }
                        }
                        if !entry.prs.is_empty() {
                            let prs: Vec<Value> = entry
                                .prs
                                .iter()
                                .map(|pr| json!({ "title": pr.title, "body": clip(&pr.body, 600) }))
                                .collect();
                            obj.insert("prs".to_string(), json!(prs));
                        }
                        if let Some(commit) = &entry.commit {
                            obj.insert("commit".to_string(), json!(commit.subject));
                        }
                        if !entry.specs.is_empty() {
                            obj.insert("specs".to_string(), json!(entry.specs));
                        }
                        Value::Object(obj)
                    }
                })
                .collect();
            json!({ "key": section.key, "heading": section.heading, "entries": entries_json })
        })
        .collect();

    json!({ "sections": sections_json })
}

/// The local calendar day an ISO instant falls on (shared/dates.ts's `isoDay`, read in this
/// computer's own timezone).
pub fn local_day(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

/// `frontMatterLines()` (shared/releaseNote.ts): the `---`-fenced `from`/`to`/`generated` header
/// every note starts with.
pub fn front_matter_lines(range: &Range) -> Vec<String> {
    vec![
        "---".to_string(),
        format!("from: {}", local_day(&range.from)),
        format!("to: {}", local_day(&range.to)),
        format!("generated: {}", range.generated_at),
        "---".to_string(),
        String::new(),
    ]
}

/// `noteMarkdown()` (shared/releaseNote.ts): renders the front matter, title, summary, highlights,
/// "also new"/"what's new" list and fixes list into the note's Markdown.
pub fn note_markdown(
    range: &Range,
    title: &str,
    summary: Option<&str>,
    highlights: &[(String, String)],
    also_new: &[String],
    fixes: &[String],
) -> String {
    let mut out = front_matter_lines(range);

    let highlights: Vec<&(String, String)> = highlights
        .iter()
        .filter(|(headline, _)| !headline.trim().is_empty())
        .take(MAX_HIGHLIGHTS)
        .collect();
    let also_new: Vec<String> = also_new
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let fixes: Vec<String> = fixes
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if highlights.is_empty() && also_new.is_empty() && fixes.is_empty() {
        out.push(format!(
            "Nothing new for users between {} and {}.",
            local_day(&range.from),
            local_day(&range.to)
        ));
        out.push(String::new());
        return out.join("\n");
    }

    out.push(format!("# {}", title.trim()));
    out.push(String::new());

    if let Some(summary) = summary {
        let summary = summary.trim();
        if !summary.is_empty() {
            out.push(summary.to_string());
            out.push(String::new());
        }
    }

    if !highlights.is_empty() {
        out.push("## Highlights".to_string());
        out.push(String::new());
        for (headline, text) in &highlights {
            out.push(format!("### {}", headline.trim()));
            out.push(String::new());
            let text = text.trim();
            if !text.is_empty() {
                out.push(text.to_string());
                out.push(String::new());
            }
        }
    }

    if !also_new.is_empty() {
        out.push(if highlights.is_empty() {
            "## What's new".to_string()
        } else {
            "## Also new".to_string()
        });
        out.push(String::new());
        out.extend(also_new.iter().map(|line| format!("- {line}")));
        out.push(String::new());
    }

    if !fixes.is_empty() {
        out.push("## Fixes".to_string());
        out.push(String::new());
        out.extend(fixes.iter().map(|line| format!("- {line}")));
        out.push(String::new());
    }

    out.join("\n")
}

/// `withoutFrontMatter`'s counterpart for an empty range (shared/releaseNote.ts): front matter plus
/// one line saying nothing merged.
pub fn empty_note(range: &Range) -> String {
    let mut out = front_matter_lines(range);
    out.push(format!(
        "Nothing merged between {} and {}.",
        local_day(&range.from),
        local_day(&range.to)
    ));
    out.push(String::new());
    out.join("\n")
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

/// Renders the final note from a grouping and the model's (optional) answer. With no answer, falls
/// back to a best-effort note from the facts alone (item titles, no prose) — matching
/// `releaseNotes.ts`'s behaviour when a session fails or hasn't run yet.
pub fn markdown(sections: &[Section], range: &Range, result: Option<&ReleaseNotesResult>) -> String {
    let visible = for_users(sections);
    if visible.iter().all(|s| s.entries.is_empty()) {
        return empty_note(range);
    }

    let mut section_of: HashMap<&str, &str> = HashMap::new();
    for section in &visible {
        for entry in &section.entries {
            section_of.insert(entry.key.as_str(), section.key.as_str());
        }
    }

    let texts: HashMap<&str, &str> = result
        .map(|r| {
            r.entries
                .iter()
                .filter(|e| section_of.contains_key(e.key.as_str()) && !e.text.trim().is_empty())
                .map(|e| (e.key.as_str(), e.text.trim()))
                .collect()
        })
        .unwrap_or_default();

    let feature_entries: Vec<&Entry> = visible
        .iter()
        .filter(|s| s.key.starts_with("spec:") || s.key == "other")
        .flat_map(|s| s.entries.iter())
        .collect();

    let highlight_triplets: Vec<(String, String, String)> = match result {
        Some(r) => {
            let mut seen = HashSet::new();
            r.highlights
                .iter()
                .filter(|h| {
                    section_of.contains_key(h.key.as_str())
                        && !h.headline.trim().is_empty()
                        && seen.insert(h.key.clone())
                })
                .take(MAX_HIGHLIGHTS)
                .map(|h| (h.key.clone(), h.headline.clone(), h.text.clone()))
                .collect()
        }
        None => feature_entries
            .iter()
            .filter(|e| {
                matches!(
                    e.item.as_ref().map(|i| i.status),
                    Some(ItemStatus::OnMain) | Some(ItemStatus::InProduction)
                )
            })
            .take(MAX_HIGHLIGHTS)
            .map(|e| (e.key.clone(), title_of(e), String::new()))
            .collect(),
    };

    let highlighted_keys: HashSet<String> = highlight_triplets.iter().map(|(key, _, _)| key.clone()).collect();
    let highlights: Vec<(String, String)> = highlight_triplets
        .into_iter()
        .map(|(_, headline, text)| (headline, text))
        .collect();

    let also_new: Vec<String> = feature_entries
        .iter()
        .filter(|e| !highlighted_keys.contains(e.key.as_str()))
        .map(|e| {
            texts
                .get(e.key.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| title_of(e))
        })
        .collect();

    // Fixes (the `playtest`-keyed section) aren't split out of chores in this slice; see the module
    // doc comment.
    let fixes: Vec<String> = Vec::new();

    let title = result
        .map(|r| r.title.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("What's new: {} to {}", local_day(&range.from), local_day(&range.to)));
    let summary = result.map(|r| r.summary.clone());

    note_markdown(range, &title, summary.as_deref(), &highlights, &also_new, &fixes)
}

/// The words `{{projectKind}}`/`{{projectNoun}}`/`{{projectUser}}` fill in, one set per project type
/// (shared/projectType.ts's `WORDS` + agent/src/prompts.ts's `PROMPT_WORDS`, combined).
pub struct ProjectWords {
    pub kind: &'static str,
    pub noun: &'static str,
    pub user: &'static str,
}

pub fn project_words(project_type: ProjectType) -> ProjectWords {
    match project_type {
        ProjectType::Game => ProjectWords {
            kind: "a game",
            noun: "the game",
            user: "a player",
        },
        ProjectType::Web => ProjectWords {
            kind: "a web app",
            noun: "the web app",
            user: "a user",
        },
        ProjectType::Server => ProjectWords {
            kind: "a server or API",
            noun: "the server",
            user: "a client or caller",
        },
        ProjectType::Cli => ProjectWords {
            kind: "a command-line program or library",
            noun: "the program",
            user: "a user",
        },
        ProjectType::Other => ProjectWords {
            kind: "an app",
            noun: "the app",
            user: "a user",
        },
    }
}

/// `render()` (agent/src/prompts.ts): fills `{{name}}` placeholders, failing on one the caller didn't
/// supply rather than leaving it in or silently dropping it.
pub fn render_prompt(template: &str, vars: &[(&str, String)]) -> Result<String> {
    let map: HashMap<&str, &str> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let placeholder = Regex::new(r"\{\{(\w+)\}\}").unwrap();
    let mut missing: Option<String> = None;
    let out = placeholder.replace_all(template, |caps: &regex::Captures| match map.get(&caps[1]) {
        Some(value) => value.to_string(),
        None => {
            missing = Some(caps[1].to_string());
            String::new()
        }
    });
    match missing {
        Some(key) => anyhow::bail!("prompt is missing the {{{{{key}}}}} variable"),
        None => Ok(out.into_owned()),
    }
}

/// Fills `prompts/release-notes.md`'s template with this project's words, range and facts.
pub fn build_prompt(
    template: &str,
    project_type: ProjectType,
    base_branch: &str,
    github_repo: &str,
    range: &Range,
    facts: &Value,
) -> Result<String> {
    let words = project_words(project_type);
    let facts_text = serde_json::to_string_pretty(facts).context("rendering release notes facts as json")?;
    render_prompt(
        template,
        &[
            ("projectKind", words.kind.to_string()),
            ("projectNoun", words.noun.to_string()),
            ("projectUser", words.user.to_string()),
            ("baseBranch", base_branch.to_string()),
            ("githubRepo", github_repo.to_string()),
            ("from", local_day(&range.from)),
            ("to", local_day(&range.to)),
            ("facts", facts_text),
        ],
    )
}

/// Reads each named spec file at the base branch (`git show <ref>:<path>`), for titling its section.
/// A file that can't be read (moved, deleted, bad ref) maps to `None` rather than failing the whole
/// gather — [`spec_title`] falls back to the file name.
pub async fn read_base_files(
    git_bin: &Path,
    cwd: &Path,
    base_ref: &str,
    paths: &[String],
) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    for path in paths {
        let output = Command::new(git_bin)
            .args(["show", &format!("{base_ref}:{path}")])
            .current_dir(cwd)
            .output()
            .await;
        let content = match output {
            Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
            _ => None,
        };
        out.insert(path.clone(), content);
    }
    out
}

/// `second()` (agent/src/codeHost/gh.ts): `gh`'s `--search merged:` range drops a trailing
/// millisecond fraction before the `Z`.
fn trim_millis(t: &str) -> String {
    let Some(without_z) = t.strip_suffix('Z') else {
        return t.to_string();
    };
    let Some(dot) = without_z.rfind('.') else {
        return t.to_string();
    };
    let fraction = &without_z[dot + 1..];
    if fraction.is_empty() || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return t.to_string();
    }
    format!("{}Z", &without_z[..dot])
}

/// `mergedPrArgs()` (agent/src/codeHost/gh.ts): the `gh pr list` arguments for the PRs merged into
/// `base_branch` between `from` and `to`.
pub fn merged_pr_args(repo: &str, base_branch: &str, from: &str, to: &str, limit: u32) -> Vec<String> {
    vec![
        "pr".to_string(),
        "list".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--base".to_string(),
        base_branch.to_string(),
        "--state".to_string(),
        "merged".to_string(),
        "--search".to_string(),
        format!("merged:{}..{}", trim_millis(from), trim_millis(to)),
        "--limit".to_string(),
        limit.to_string(),
        "--json".to_string(),
        "number,title,url,body,mergedAt,headRefName,mergeCommit".to_string(),
    ]
}

#[derive(Deserialize)]
struct RawMergeCommit {
    oid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMergedPr {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    body: String,
    merged_at: String,
    head_ref_name: String,
    #[serde(default)]
    merge_commit: Option<RawMergeCommit>,
}

impl From<RawMergedPr> for MergedPr {
    fn from(r: RawMergedPr) -> Self {
        MergedPr {
            number: r.number,
            title: r.title,
            url: r.url,
            body: r.body,
            merged_at: r.merged_at,
            head_ref_name: r.head_ref_name,
            merge_commit_oid: r.merge_commit.map(|m| m.oid),
        }
    }
}

/// Runs `gh pr list` for the PRs merged into `base_branch` between `from` and `to`, up to `limit`.
pub async fn gather_merged_prs(
    gh_bin: &Path,
    cwd: &Path,
    repo: &str,
    base_branch: &str,
    from: &str,
    to: &str,
    limit: u32,
) -> Result<Vec<MergedPr>> {
    let args = merged_pr_args(repo, base_branch, from, to, limit);
    let output = Command::new(gh_bin)
        .args(&args)
        .current_dir(cwd)
        .output()
        .await
        .context("running gh pr list")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!("gh pr list failed: {}", clip(&stderr, 300));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw: Vec<RawMergedPr> = if stdout.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&stdout).context("parsing gh pr list output")?
    };
    Ok(raw.into_iter().map(MergedPr::from).collect())
}

/// `commitLogArgs()`-equivalent (agent/src/git.ts's merged-commit gathering): `git log` arguments for
/// the first-parent history of `base_ref` between `from` and `to`.
pub fn commit_log_args(base_ref: &str, from: &str, to: &str) -> Vec<String> {
    vec![
        "log".to_string(),
        "--first-parent".to_string(),
        "--format=%H%x09%cI%x09%an%x09%s".to_string(),
        format!("--since={from}"),
        format!("--until={to}"),
        base_ref.to_string(),
    ]
}

/// Runs `git log --first-parent` for `base_ref` between `from` and `to`. A failing `git log` (bad
/// ref, no repo) reads as no commits rather than an error — the PRs gathered separately still stand.
pub async fn gather_commits(git_bin: &Path, cwd: &Path, base_ref: &str, from: &str, to: &str) -> Result<Vec<Commit>> {
    let args = commit_log_args(base_ref, from, to);
    let output = Command::new(git_bin)
        .args(&args)
        .current_dir(cwd)
        .output()
        .await
        .context("running git log")?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(|line| {
            let mut parts = line.splitn(4, '\t');
            Commit {
                sha: parts.next().unwrap_or_default().to_string(),
                date: parts.next().unwrap_or_default().to_string(),
                author: parts.next().unwrap_or_default().to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
            }
        })
        .collect())
}

/// The inputs [`gather`] needs, grouped so the function itself takes one argument instead of nine.
pub struct GatherArgs<'a> {
    pub gh_bin: &'a Path,
    pub git_bin: &'a Path,
    pub cwd: &'a Path,
    pub github_repo: &'a str,
    pub base_branch: &'a str,
    pub base_ref: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub max_prs: u32,
}

/// Gathers the merged PRs and commits for a range, refusing a range with more than `max_prs` merged
/// PRs rather than guessing which to drop (asks for one more than `max_prs` to tell "exactly at the
/// limit" apart from "over it" in a single `gh` call).
pub async fn gather(args: GatherArgs<'_>) -> Result<(Vec<MergedPr>, Vec<Commit>)> {
    let prs = gather_merged_prs(
        args.gh_bin,
        args.cwd,
        args.github_repo,
        args.base_branch,
        args.from,
        args.to,
        args.max_prs + 1,
    )
    .await?;
    if prs.len() as u32 > args.max_prs {
        anyhow::bail!(
            "more than {} PRs merged in this range: pick a shorter one",
            args.max_prs
        );
    }
    let commits = gather_commits(args.git_bin, args.cwd, args.base_ref, args.from, args.to).await?;
    Ok((prs, commits))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn item(
        id: &str,
        title: &str,
        status: ItemStatus,
        pr_url: Option<&str>,
        spec_refs: &[&str],
        milestone: Option<&str>,
    ) -> Item {
        Item {
            id: id.to_string(),
            title: title.to_string(),
            status,
            rank: 0,
            do_next: false,
            depends_on: Vec::new(),
            spec_refs: spec_refs.iter().map(|s| s.to_string()).collect(),
            slice: 0,
            remaining: None,
            pr_url: pr_url.map(|s| s.to_string()),
            branch: None,
            kind: None,
            milestone: milestone.map(|s| s.to_string()),
        }
    }

    fn pr(number: u64, title: &str, url: &str, head_ref_name: &str, merged_at: &str) -> MergedPr {
        MergedPr {
            number,
            title: title.to_string(),
            url: url.to_string(),
            body: String::new(),
            merged_at: merged_at.to_string(),
            head_ref_name: head_ref_name.to_string(),
            merge_commit_oid: None,
        }
    }

    fn range() -> Range {
        Range {
            from: "2026-09-23T00:00:00Z".to_string(),
            to: "2026-09-30T00:00:00Z".to_string(),
            generated_at: "2026-09-30T12:00:00Z".to_string(),
        }
    }

    // pr_owner

    #[test]
    fn pr_owner_matches_by_pr_url_first() {
        let items = vec![item(
            "1",
            "A",
            ItemStatus::OnMain,
            Some("https://github.com/x/y/pull/9"),
            &[],
            None,
        )];
        let p = pr(
            9,
            "A",
            "https://github.com/x/y/pull/9",
            "retaskable/1-a",
            "2026-09-24T00:00:00Z",
        );
        assert_eq!(pr_owner(&p, &items).unwrap().id, "1");
    }

    #[test]
    fn pr_owner_falls_back_to_the_longest_branch_prefix() {
        let items = vec![
            item("1", "A", ItemStatus::OnMain, None, &[], None),
            item("12", "B", ItemStatus::OnMain, None, &[], None),
        ];
        let p = pr(
            9,
            "B",
            "https://github.com/x/y/pull/9",
            "retaskable/12-b-slice",
            "2026-09-24T00:00:00Z",
        );
        assert_eq!(pr_owner(&p, &items).unwrap().id, "12");
    }

    #[test]
    fn pr_owner_is_none_when_nothing_matches() {
        let items = vec![item("1", "A", ItemStatus::OnMain, None, &[], None)];
        let p = pr(
            9,
            "Stray",
            "https://github.com/x/y/pull/9",
            "some-other-branch",
            "2026-09-24T00:00:00Z",
        );
        assert!(pr_owner(&p, &items).is_none());
    }

    // group

    #[test]
    fn group_routes_a_chore_item_to_its_own_section() {
        let items = vec![item(
            "1",
            "Bump a dependency",
            ItemStatus::OnMain,
            None,
            &[],
            Some("chores"),
        )];
        let p = pr(
            1,
            "Bump a dependency",
            "https://github.com/x/y/pull/1",
            "retaskable/1-bump",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &items, &HashMap::new());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].key, "chores");
        assert!(sections[0].entries[0].chore);
    }

    #[test]
    fn group_routes_an_item_with_a_spec_to_a_spec_section() {
        let items = vec![item(
            "1",
            "Charts",
            ItemStatus::OnMain,
            None,
            &["docs/specs/charts.md"],
            None,
        )];
        let p = pr(
            1,
            "Add charts",
            "https://github.com/x/y/pull/1",
            "retaskable/1-charts",
            "2026-09-24T00:00:00Z",
        );
        let mut files = HashMap::new();
        files.insert(
            "docs/specs/charts.md".to_string(),
            Some("# Charts — Spec\n".to_string()),
        );
        let sections = group(&[p], &[], &items, &files);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].key, "spec:docs/specs/charts.md");
        assert_eq!(sections[0].heading, "Charts");
    }

    #[test]
    fn group_merges_two_prs_from_the_same_item_into_one_entry() {
        let items = vec![item(
            "1",
            "Charts",
            ItemStatus::OnMain,
            None,
            &["docs/specs/charts.md"],
            None,
        )];
        let p1 = pr(
            1,
            "Charts: part 1",
            "https://github.com/x/y/pull/1",
            "retaskable/1-charts",
            "2026-09-24T00:00:00Z",
        );
        let p2 = pr(
            2,
            "Charts: part 2",
            "https://github.com/x/y/pull/2",
            "retaskable/1-charts",
            "2026-09-25T00:00:00Z",
        );
        let sections = group(&[p1, p2], &[], &items, &HashMap::new());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].entries.len(), 1);
        assert_eq!(sections[0].entries[0].prs.len(), 2);
    }

    #[test]
    fn group_routes_an_unmatched_pr_to_other_by_itself() {
        let p = pr(
            1,
            "Stray fix",
            "https://github.com/x/y/pull/1",
            "someone/stray",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &[], &HashMap::new());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].key, "other");
        assert_eq!(sections[0].entries[0].key, "pr:1");
        assert_eq!(sections[0].entries[0].fallback, "Stray fix");
    }

    #[test]
    fn group_routes_a_commit_with_no_pr_to_other() {
        let commit = Commit {
            sha: "abcdef1234".to_string(),
            date: "2026-09-24T00:00:00Z".to_string(),
            author: "A".to_string(),
            subject: "Tidy".to_string(),
        };
        let sections = group(&[], &[commit], &[], &HashMap::new());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].entries[0].key, "commit:abcdef1");
    }

    #[test]
    fn group_skips_a_commit_that_is_a_prs_merge_commit() {
        let mut p = pr(
            1,
            "Charts",
            "https://github.com/x/y/pull/1",
            "someone/charts",
            "2026-09-24T00:00:00Z",
        );
        p.merge_commit_oid = Some("abcdef1234".to_string());
        let commit = Commit {
            sha: "abcdef1234".to_string(),
            date: "2026-09-24T00:00:00Z".to_string(),
            author: "A".to_string(),
            subject: "Charts".to_string(),
        };
        let sections = group(&[p], &[commit], &[], &HashMap::new());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].entries.len(), 1);
        assert_eq!(sections[0].entries[0].key, "pr:1");
    }

    #[test]
    fn group_drops_empty_sections() {
        let sections = group(&[], &[], &[], &HashMap::new());
        assert!(sections.is_empty());
    }

    // counts / for_users

    #[test]
    fn counts_reports_distinct_items_and_chores() {
        let items = vec![
            item("1", "Charts", ItemStatus::OnMain, None, &["docs/specs/charts.md"], None),
            item("2", "Bump", ItemStatus::OnMain, None, &[], Some("chores")),
        ];
        let p1 = pr(
            1,
            "Charts",
            "https://github.com/x/y/pull/1",
            "retaskable/1-charts",
            "2026-09-24T00:00:00Z",
        );
        let p2 = pr(
            2,
            "Charts again",
            "https://github.com/x/y/pull/2",
            "retaskable/1-charts",
            "2026-09-25T00:00:00Z",
        );
        let p3 = pr(
            3,
            "Bump",
            "https://github.com/x/y/pull/3",
            "retaskable/2-bump",
            "2026-09-26T00:00:00Z",
        );
        let sections = group(&[p1, p2, p3], &[], &items, &HashMap::new());
        let c = counts(&sections, 3);
        assert_eq!(c.prs, 3);
        assert_eq!(c.items, 2);
        assert_eq!(c.chores, 1);
    }

    #[test]
    fn for_users_drops_the_chores_section() {
        let items = vec![item("1", "Bump", ItemStatus::OnMain, None, &[], Some("chores"))];
        let p = pr(
            1,
            "Bump",
            "https://github.com/x/y/pull/1",
            "retaskable/1-bump",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &items, &HashMap::new());
        assert!(for_users(&sections).is_empty());
    }

    // clip

    #[test]
    fn clip_leaves_short_text_alone() {
        assert_eq!(clip("hello", 10), "hello");
    }

    #[test]
    fn clip_truncates_and_adds_an_ellipsis() {
        assert_eq!(clip("hello world", 5), "hell…");
    }

    // facts

    #[test]
    fn facts_keeps_a_small_section_in_full() {
        let items = vec![item(
            "1",
            "Charts",
            ItemStatus::OnMain,
            None,
            &["docs/specs/charts.md"],
            None,
        )];
        let p = pr(
            1,
            "Add charts",
            "https://github.com/x/y/pull/1",
            "retaskable/1-charts",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &items, &HashMap::new());
        let f = facts(&sections, 120);
        let entry = &f["sections"][0]["entries"][0];
        assert_eq!(entry["item"], "Charts");
        assert_eq!(entry["titleOnly"], Value::Null);
    }

    #[test]
    fn facts_trims_entries_past_the_max_to_title_only() {
        let items: Vec<Item> = (0..5)
            .map(|i| {
                item(
                    &i.to_string(),
                    &format!("Item {i}"),
                    ItemStatus::OnMain,
                    None,
                    &[],
                    None,
                )
            })
            .collect();
        let prs: Vec<MergedPr> = (0..5)
            .map(|i| {
                pr(
                    i,
                    &format!("PR {i}"),
                    &format!("https://github.com/x/y/pull/{i}"),
                    &format!("retaskable/{i}-x"),
                    "2026-09-24T00:00:00Z",
                )
            })
            .collect();
        let sections = group(&prs, &[], &items, &HashMap::new());
        let f = facts(&sections, 2);
        let total_title_only: usize = f["sections"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["entries"].as_array().unwrap().iter())
            .filter(|e| e["titleOnly"] == true)
            .count();
        assert!(total_title_only > 0);
    }

    // local_day / front_matter_lines

    #[test]
    fn local_day_reads_the_calendar_day_of_an_iso_instant() {
        let day = local_day("2026-09-24T00:00:00Z");
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
    }

    #[test]
    fn front_matter_lines_has_the_from_to_and_generated_keys() {
        let lines = front_matter_lines(&range());
        assert_eq!(lines[0], "---");
        assert!(lines[1].starts_with("from: "));
        assert!(lines[2].starts_with("to: "));
        assert_eq!(lines[3], "generated: 2026-09-30T12:00:00Z");
        assert_eq!(lines[4], "---");
    }

    // note_markdown / empty_note

    #[test]
    fn note_markdown_says_nothing_new_when_everything_is_empty() {
        let md = note_markdown(&range(), "Title", None, &[], &[], &[]);
        assert!(md.contains("Nothing new for users between"));
        assert!(!md.contains("# Title"));
    }

    #[test]
    fn note_markdown_renders_highlights_also_new_and_fixes() {
        let md = note_markdown(
            &range(),
            "My App: this week",
            Some("A good week."),
            &[("Charts".to_string(), "You can now chart things.".to_string())],
            &["Small thing".to_string()],
            &["Fixed a crash".to_string()],
        );
        assert!(md.contains("# My App: this week"));
        assert!(md.contains("A good week."));
        assert!(md.contains("## Highlights"));
        assert!(md.contains("### Charts"));
        assert!(md.contains("## Also new"));
        assert!(md.contains("- Small thing"));
        assert!(md.contains("## Fixes"));
        assert!(md.contains("- Fixed a crash"));
    }

    #[test]
    fn note_markdown_titles_the_also_new_section_whats_new_with_no_highlights() {
        let md = note_markdown(&range(), "Title", None, &[], &["A thing".to_string()], &[]);
        assert!(md.contains("## What's new"));
        assert!(!md.contains("## Also new"));
    }

    #[test]
    fn empty_note_says_nothing_merged() {
        let note = empty_note(&range());
        assert!(note.contains("Nothing merged between"));
    }

    // markdown

    #[test]
    fn markdown_falls_back_to_done_item_titles_with_no_model_result() {
        let items = vec![item(
            "1",
            "Charts",
            ItemStatus::OnMain,
            None,
            &["docs/specs/charts.md"],
            None,
        )];
        let p = pr(
            1,
            "Add charts",
            "https://github.com/x/y/pull/1",
            "retaskable/1-charts",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &items, &HashMap::new());
        let md = markdown(&sections, &range(), None);
        assert!(md.contains("### 1 Charts"));
    }

    #[test]
    fn markdown_uses_the_models_title_summary_and_highlight_text() {
        let items = vec![item(
            "1",
            "Charts",
            ItemStatus::OnMain,
            None,
            &["docs/specs/charts.md"],
            None,
        )];
        let p = pr(
            1,
            "Add charts",
            "https://github.com/x/y/pull/1",
            "retaskable/1-charts",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &items, &HashMap::new());
        let result = ReleaseNotesResult {
            status: "ok".to_string(),
            title: "My App: this week".to_string(),
            summary: "A good week.".to_string(),
            highlights: vec![Highlight {
                key: "item:1".to_string(),
                headline: "Charts for everyone".to_string(),
                text: "Chart your data.".to_string(),
            }],
            entries: vec![],
            error: None,
        };
        let md = markdown(&sections, &range(), Some(&result));
        assert!(md.contains("# My App: this week"));
        assert!(md.contains("A good week."));
        assert!(md.contains("### Charts for everyone"));
        assert!(md.contains("Chart your data."));
    }

    #[test]
    fn markdown_is_the_empty_note_with_nothing_for_users() {
        let items = vec![item("1", "Bump", ItemStatus::OnMain, None, &[], Some("chores"))];
        let p = pr(
            1,
            "Bump",
            "https://github.com/x/y/pull/1",
            "retaskable/1-bump",
            "2026-09-24T00:00:00Z",
        );
        let sections = group(&[p], &[], &items, &HashMap::new());
        let md = markdown(&sections, &range(), None);
        assert!(md.contains("Nothing merged between"));
    }

    // project_words

    #[test]
    fn project_words_covers_every_project_type() {
        for t in [
            ProjectType::Game,
            ProjectType::Web,
            ProjectType::Server,
            ProjectType::Cli,
            ProjectType::Other,
        ] {
            let words = project_words(t);
            assert!(!words.kind.is_empty());
            assert!(!words.noun.is_empty());
            assert!(!words.user.is_empty());
        }
    }

    // render_prompt / build_prompt

    #[test]
    fn render_prompt_fills_every_placeholder() {
        let out = render_prompt(
            "Hello {{name}}, welcome to {{place}}.",
            &[("name", "A".to_string()), ("place", "B".to_string())],
        )
        .unwrap();
        assert_eq!(out, "Hello A, welcome to B.");
    }

    #[test]
    fn render_prompt_fails_on_a_placeholder_with_no_value() {
        let err = render_prompt("Hello {{name}}.", &[]).unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn build_prompt_fills_the_release_notes_template() {
        let template = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("release-notes.md"),
        )
        .unwrap();
        let prompt = build_prompt(
            &template,
            ProjectType::Web,
            "main",
            "x/y",
            &range(),
            &json!({"sections": []}),
        )
        .unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("a web app"));
        assert!(prompt.contains("main"));
        assert!(prompt.contains("x/y"));
    }

    // spec_title

    #[test]
    fn spec_title_strips_a_trailing_spec_suffix() {
        assert_eq!(
            spec_title("docs/specs/charts.md", Some("# Charts — Spec\n\nBody.")),
            "Charts"
        );
    }

    #[test]
    fn spec_title_falls_back_to_the_file_name_with_no_heading() {
        assert_eq!(spec_title("docs/specs/charts.md", None), "charts");
    }

    // merged_pr_args / commit_log_args

    #[test]
    fn merged_pr_args_builds_the_documented_gh_search() {
        let args = merged_pr_args("x/y", "main", "2026-09-23T00:00:00.000Z", "2026-09-30T00:00:00.000Z", 5);
        assert_eq!(args[0], "pr");
        assert_eq!(args[1], "list");
        let search_idx = args.iter().position(|a| a == "--search").unwrap();
        assert_eq!(
            args[search_idx + 1],
            "merged:2026-09-23T00:00:00Z..2026-09-30T00:00:00Z"
        );
    }

    #[test]
    fn commit_log_args_uses_first_parent_and_the_tab_separated_format() {
        let args = commit_log_args("origin/main", "2026-09-23T00:00:00Z", "2026-09-30T00:00:00Z");
        assert!(args.contains(&"--first-parent".to_string()));
        assert!(args.iter().any(|a| a == "--format=%H%x09%cI%x09%an%x09%s"));
        assert_eq!(args.last().unwrap(), "origin/main");
    }

    // gather_merged_prs / gather_commits (a fake `gh`/`git` binary that ignores its arguments and
    // just prints a fixture file, like session.rs's own fake-session tests do for `claude`)

    #[tokio::test]
    async fn gather_merged_prs_parses_ghs_json_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("prs.json");
        std::fs::write(
            &out,
            r#"[{"number":1,"title":"Add charts","url":"https://github.com/x/y/pull/1","body":"body","mergedAt":"2026-09-24T00:00:00Z","headRefName":"retaskable/1-charts","mergeCommit":{"oid":"abc123"}}]"#,
        )
        .unwrap();
        let fake_gh = dir
            .path()
            .join(if cfg!(windows) { "fake_gh.cmd" } else { "fake_gh.sh" });
        write_fake_binary_printing(&fake_gh, &out);
        let prs = gather_merged_prs(
            &fake_gh,
            dir.path(),
            "x/y",
            "main",
            "2026-09-23T00:00:00Z",
            "2026-09-30T00:00:00Z",
            10,
        )
        .await
        .unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 1);
        assert_eq!(prs[0].merge_commit_oid, Some("abc123".to_string()));
    }

    #[tokio::test]
    async fn gather_commits_parses_gits_tab_separated_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("log.txt");
        std::fs::write(&out, "abc123\t2026-09-24T00:00:00Z\tA Dev\tTidy things up\n").unwrap();
        let fake_git = dir
            .path()
            .join(if cfg!(windows) { "fake_git.cmd" } else { "fake_git.sh" });
        write_fake_binary_printing(&fake_git, &out);
        let commits = gather_commits(
            &fake_git,
            dir.path(),
            "origin/main",
            "2026-09-23T00:00:00Z",
            "2026-09-30T00:00:00Z",
        )
        .await
        .unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].subject, "Tidy things up");
    }

    fn write_fake_binary_printing(path: &Path, fixture: &Path) {
        if cfg!(windows) {
            std::fs::write(path, format!("@type \"{}\"\r\n", fixture.display())).unwrap();
        } else {
            std::fs::write(path, format!("#!/bin/sh\ncat \"{}\"\n", fixture.display())).unwrap();
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                perms.set_mode(0o755);
            }
            std::fs::set_permissions(path, perms).unwrap();
        }
    }
}
