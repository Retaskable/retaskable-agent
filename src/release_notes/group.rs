//! Groups gathered PRs and commits by the backlog item that owns them, and builds the JSON "facts"
//! the release-notes prompt is built from.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};

use crate::backlog::{Item, ItemStatus};

use super::model::{Commit, Counts, Entry, ItemRef, MergedPr, PrRef, Section};

/// `releaseFullEntries`'s default (shared/tuning.ts): entries beyond this many are trimmed to just
/// their title before they go to the model.
pub const DEFAULT_FULL_ENTRIES: usize = 120;

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
/// it couldn't be read), from [`super::gather::read_base_files`], used to title that spec's section.
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

pub(super) fn title_of(entry: &Entry) -> String {
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

#[cfg(test)]
mod tests {
    use crate::release_notes::test_support::{charts_item_and_pr, item, pr};

    use super::*;

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
        let (charts_item, p) = charts_item_and_pr();
        let mut files = HashMap::new();
        files.insert(
            "docs/specs/charts.md".to_string(),
            Some("# Charts — Spec\n".to_string()),
        );
        let sections = group(&[p], &[], &[charts_item], &files);
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
        let (charts_item, p) = charts_item_and_pr();
        let sections = group(&[p], &[], &[charts_item], &HashMap::new());
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
}
