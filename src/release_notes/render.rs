//! Turns a grouping (and the model's optional structured answer) into the note's final Markdown,
//! and builds the `prompts/release-notes.md` prompt a release-notes session is run with.
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::backlog::ItemStatus;
use crate::config::ProjectType;

use super::group::for_users;
use super::model::{Entry, Range, ReleaseNotesResult, Section};
use crate::prompt::{project_words, render_prompt};

/// `MAX_HIGHLIGHTS` (shared/releaseNote.ts).
pub const MAX_HIGHLIGHTS: usize = 5;

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
    out.push(format!("# {}", title.trim()));
    out.push(String::new());

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

/// Renders the final note from a grouping and the model's (optional) answer. With no answer, falls
/// back to a best-effort note from the facts alone (item titles, no prose) — matching
/// `releaseNotes.ts`'s behaviour when a session fails or hasn't run yet.
pub fn markdown(sections: &[Section], range: &Range, result: Option<&ReleaseNotesResult>) -> String {
    let visible = for_users(sections);

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
            .filter_map(|e| e.item.as_ref().map(|item| (e, item)))
            .filter(|(_, item)| matches!(item.status, ItemStatus::OnMain | ItemStatus::InProduction))
            .take(MAX_HIGHLIGHTS)
            .map(|(e, item)| (e.key.clone(), item.title.clone(), String::new()))
            .collect(),
    };

    let highlighted_keys: HashSet<String> = highlight_triplets.iter().map(|(key, _, _)| key.clone()).collect();
    let highlights: Vec<(String, String)> = highlight_triplets
        .into_iter()
        .map(|(_, headline, text)| (headline, text))
        .collect();

    // Another change only shows when the model wrote a line for it (a dependency bump means nothing
    // to the readers); an item it left out is listed by its title.
    let also_new: Vec<String> = feature_entries
        .iter()
        .filter(|e| !highlighted_keys.contains(e.key.as_str()))
        .filter_map(|e| {
            texts
                .get(e.key.as_str())
                .map(|s| s.to_string())
                .or_else(|| e.item.as_ref().map(|item| item.title.clone()))
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use crate::release_notes::model::{Highlight, NoteEntry};
    use crate::release_notes::test_support::{charts_item_and_pr, grouped, item, pr, range};

    use super::*;

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
        assert!(md.contains("# Title\n\nNothing new for users between"));
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
        let (charts_item, p) = charts_item_and_pr();
        let sections = grouped(&[p], &[], &[charts_item], &HashMap::new());
        let md = markdown(&sections, &range(), None);
        assert!(md.contains("### Charts\n"));
        assert!(!md.contains("### 1 Charts"));
    }

    #[test]
    fn markdown_lists_an_item_by_its_bare_title_and_drops_another_change_with_no_line() {
        let items = vec![item("7", "Phones", ItemStatus::Building, None, &[], None)];
        let prs = [
            pr(
                1,
                "Phone work",
                "https://github.com/x/y/pull/1",
                "retaskable/7-phones",
                "2026-09-24T00:00:00Z",
            ),
            pr(
                2,
                "Bump serde",
                "https://github.com/x/y/pull/2",
                "someone/bump",
                "2026-09-25T00:00:00Z",
            ),
            pr(
                3,
                "Fix typo",
                "https://github.com/x/y/pull/3",
                "someone/typo",
                "2026-09-26T00:00:00Z",
            ),
        ];
        let sections = grouped(&prs, &[], &items, &HashMap::new());
        let result = ReleaseNotesResult {
            status: "ok".to_string(),
            title: "This week".to_string(),
            summary: String::new(),
            highlights: vec![],
            entries: vec![NoteEntry {
                key: "pr:3".to_string(),
                text: "Fixed a typo on the start screen".to_string(),
            }],
            error: None,
        };
        let md = markdown(&sections, &range(), Some(&result));
        assert!(md.contains("## What's new\n\n- Phones\n- Fixed a typo on the start screen\n"));
        assert!(!md.contains("Bump serde"));
        assert!(!md.contains("7 Phones"));

        let md = markdown(&sections, &range(), None);
        assert!(md.contains("- Phones\n"));
        assert!(!md.contains("Bump serde"));
        assert!(!md.contains("Fix typo"));
    }

    #[test]
    fn markdown_uses_the_models_title_summary_and_highlight_text() {
        let (charts_item, p) = charts_item_and_pr();
        let sections = grouped(&[p], &[], &[charts_item], &HashMap::new());
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
    fn markdown_says_nothing_new_for_users_for_a_range_of_only_chores() {
        let items = vec![item("1", "Bump", ItemStatus::OnMain, None, &[], Some("chores"))];
        let p = pr(
            1,
            "Bump",
            "https://github.com/x/y/pull/1",
            "retaskable/1-bump",
            "2026-09-24T00:00:00Z",
        );
        let sections = grouped(&[p], &[], &items, &HashMap::new());
        let md = markdown(&sections, &range(), None);
        assert!(md.contains("# What's new: "));
        assert!(md.contains("Nothing new for users between"));
        assert!(!md.contains("Nothing merged"));
    }

    // build_prompt

    #[test]
    fn build_prompt_fills_the_release_notes_template() {
        let template = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("release-notes.md"),
        )
        .unwrap();
        let prompt = build_prompt(&template, ProjectType::Web, "main", "x/y", &range(), &json!([])).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("a web app"));
        assert!(prompt.contains("main"));
        assert!(prompt.contains("x/y"));
    }
}
