//! Groups gathered PRs and commits by the backlog item that owns them, and builds the JSON "facts"
//! the release-notes prompt is built from.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};

use crate::backlog::{Draft, Item, ItemStatus};

use super::model::{Commit, Counts, Entry, ItemRef, MergedPr, PrRef, Section, SpecLink};

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

fn new_section(key: &str, heading: &str) -> Section {
    Section {
        key: key.to_string(),
        heading: heading.to_string(),
        entries: Vec::new(),
    }
}

/// The spec's own title: its first `#` heading with a trailing "— Spec" (any dash, case-insensitive)
/// dropped, else the file's name (`releaseNotes.ts`'s `specTitle`).
pub fn spec_title(path: &str, content: Option<&str>) -> String {
    if let Some(content) = content {
        let heading = Regex::new(r"(?m)^#\s+(.+?)\s*$").unwrap();
        if let Some(caps) = heading.captures(content) {
            let suffix = RegexBuilder::new(r"\s+[-—–]\s+spec$")
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

/// What [`group`] works from: the gathered PRs and commits, the store's items and spec drafts, each
/// named spec's content at the base branch (`None` when it couldn't be read, from
/// [`super::gather::read_base_files`]), and the repo and branch its links point at.
pub struct GroupInput<'a> {
    pub prs: &'a [MergedPr],
    pub commits: &'a [Commit],
    pub items: &'a [Item],
    pub drafts: &'a [Draft],
    pub files: &'a HashMap<String, Option<String>>,
    pub github_repo: &'a str,
    pub base_branch: &'a str,
}

/// `encodeURIComponent` for one path segment.
fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn is_markdown(path: &str) -> bool {
    path.to_lowercase().ends_with(".md")
}

/// The links for an item's specs (`releaseNotes.ts`'s `itemSpecs`/`specLinks`): each Markdown file its
/// `spec_refs` names, once, at its page on the base branch, labelled with its file name. The store's
/// `specRefs` name whole files, so a link is to the file, not a section in it.
pub fn spec_links(spec_refs: &[String], github_repo: &str, base_branch: &str) -> Vec<SpecLink> {
    let mut seen = HashSet::new();
    spec_refs
        .iter()
        .filter(|r| is_markdown(r) && seen.insert(r.as_str()))
        .map(|r| {
            let encoded: Vec<String> = r.split('/').map(encode_segment).collect();
            SpecLink {
                label: r.rsplit('/').next().unwrap_or(r).to_string(),
                url: format!(
                    "https://github.com/{github_repo}/blob/{base_branch}/{}",
                    encoded.join("/")
                ),
            }
        })
        .collect()
}

/// Puts `entry` in the section for `spec`, adding that section (titled from the spec's content) the
/// first time.
fn push_under_spec(sections: &mut Vec<Section>, files: &HashMap<String, Option<String>>, spec: &str, entry: Entry) {
    let key = format!("spec:{spec}");
    let at = match sections.iter().position(|s| s.key == key) {
        Some(at) => at,
        None => {
            let heading = spec_title(spec, files.get(spec).and_then(|c| c.as_deref()));
            sections.push(new_section(&key, &heading));
            sections.len() - 1
        }
    };
    sections[at].entries.push(entry);
}

/// Groups merged PRs and commits by the backlog item that owns them (`releaseNotes.ts`'s `group`):
/// one section per spec, in the order of its first item in the backlog, then chores, then other
/// changes. A merged spec draft's PR (the store's `drafts`, by `prUrl`) goes under the spec it adds or
/// edits (the first Markdown file it changed) as "Specced: <its titles>"; one that changed none is
/// another change. Simplified where this agent's `Item` carries less: chores are the `"chores"`
/// milestone and land in one section (no playtest-fix split), an item's spec is its first Markdown
/// `spec_refs` entry, and "done" is its status alone.
pub fn group(inp: &GroupInput) -> Vec<Section> {
    let mut sorted: Vec<&MergedPr> = inp.prs.iter().collect();
    sorted.sort_by(|a, b| a.merged_at.cmp(&b.merged_at));

    let mut spec_sections: Vec<Section> = Vec::new();
    let mut chores = new_section("chores", "Chores");
    let mut other = new_section("other", "Other changes");

    // Backlog items and chores, one entry each with all their PRs in the range.
    let mut by_item: HashMap<&str, Vec<&MergedPr>> = HashMap::new();
    let mut rest: Vec<&MergedPr> = Vec::new();
    for pr in &sorted {
        match pr_owner(pr, inp.items) {
            Some(item) => by_item.entry(item.id.as_str()).or_default().push(pr),
            None => rest.push(pr),
        }
    }
    for item in inp.items {
        let Some(its) = by_item.get(item.id.as_str()) else {
            continue;
        };
        let prs: Vec<PrRef> = its.iter().map(|pr| to_pr_ref(pr)).collect();
        if item.milestone.as_deref() == Some("chores") {
            chores.entries.push(Entry {
                key: format!("item:{}", item.id),
                item: None,
                fallback: item.title.clone(),
                prs,
                commit: None,
                specs: Vec::new(),
                chore: true,
            });
            continue;
        }
        let entry = Entry {
            key: format!("item:{}", item.id),
            item: Some(ItemRef {
                id: item.id.clone(),
                title: item.title.clone(),
                status: item.status,
            }),
            fallback: its.iter().map(|pr| pr.title.as_str()).collect::<Vec<_>>().join("; "),
            prs,
            commit: None,
            specs: spec_links(&item.spec_refs, inp.github_repo, inp.base_branch),
            chore: false,
        };
        match item.spec_refs.iter().find(|r| is_markdown(r)) {
            Some(spec) => push_under_spec(&mut spec_sections, inp.files, spec, entry),
            None => other.entries.push(entry),
        }
    }
    // Spec drafts go under the spec they add or edit; anything else is someone else's.
    for pr in rest {
        let draft = inp.drafts.iter().find(|d| d.pr_url.as_deref() == Some(pr.url.as_str()));
        let spec = pr.files.iter().find(|f| is_markdown(f));
        if let (Some(draft), Some(spec)) = (draft, spec) {
            let title = if draft.titles.is_empty() {
                pr.title.clone()
            } else {
                draft.titles.join("; ")
            };
            let entry = Entry {
                key: format!("draft:{}", draft.id),
                item: None,
                fallback: format!("Specced: {title}"),
                prs: vec![to_pr_ref(pr)],
                commit: None,
                specs: spec_links(std::slice::from_ref(spec), inp.github_repo, inp.base_branch),
                chore: false,
            };
            push_under_spec(&mut spec_sections, inp.files, spec, entry);
            continue;
        }
        other.entries.push(Entry {
            key: format!("pr:{}", pr.number),
            item: None,
            fallback: pr.title.clone(),
            prs: vec![to_pr_ref(pr)],
            commit: None,
            specs: Vec::new(),
            chore: false,
        });
    }

    // Commits pushed without a PR.
    let merged: HashSet<&str> = sorted.iter().filter_map(|pr| pr.merge_commit_oid.as_deref()).collect();
    for commit in inp.commits {
        if merged.contains(commit.sha.as_str()) {
            continue;
        }
        other.entries.push(Entry {
            key: format!("commit:{}", &commit.sha[..commit.sha.len().min(7)]),
            item: None,
            fallback: commit.subject.clone(),
            prs: Vec::new(),
            commit: Some(commit.clone()),
            specs: Vec::new(),
            chore: false,
        });
    }

    spec_sections.push(chores);
    spec_sections.push(other);
    spec_sections.retain(|s| !s.entries.is_empty());
    spec_sections
}

/// How many PRs, items and chores a grouping covers (`releaseNotes.ts`'s `counts`: an item has one
/// entry, and a chore's entry isn't an item's).
pub fn counts(sections: &[Section], total_prs: usize) -> Counts {
    let entries = || sections.iter().flat_map(|s| &s.entries);
    Counts {
        prs: total_prs,
        items: entries().filter(|e| e.item.is_some()).count(),
        chores: entries().filter(|e| e.chore).count(),
    }
}

/// The sections a release note shows people (`releaseNotes.ts`'s `forUsers`): everything but chores
/// and spec drafts, which are the team's; a section left with nothing isn't shown.
pub fn for_users(sections: &[Section]) -> Vec<Section> {
    sections
        .iter()
        .filter(|s| s.key != "chores")
        .map(|s| Section {
            entries: s
                .entries
                .iter()
                .filter(|e| !e.key.starts_with("draft:"))
                .cloned()
                .collect(),
            ..s.clone()
        })
        .filter(|s| !s.entries.is_empty())
        .collect()
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
        let section = &visible[i];
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
                        return json!({ "key": entry.key, "title": title_of(entry), "titleOnly": true });
                    }
                    let mut obj = serde_json::Map::new();
                    obj.insert("key".to_string(), json!(entry.key));
                    if let Some(item) = &entry.item {
                        obj.insert("item".to_string(), json!(title_of(entry)));
                        obj.insert("status".to_string(), json!(status_word(item.status)));
                    }
                    let prs: Vec<Value> = entry
                        .prs
                        .iter()
                        .map(|pr| json!({ "title": pr.title, "body": clip(&pr.body, 600) }))
                        .collect();
                    obj.insert("prs".to_string(), json!(prs));
                    if entry.commit.is_some() {
                        obj.insert("commit".to_string(), json!(entry.fallback));
                    }
                    let specs: Vec<&str> = entry.specs.iter().map(|s| s.label.as_str()).collect();
                    obj.insert("specs".to_string(), json!(specs));
                    Value::Object(obj)
                })
                .collect();
            json!({ "key": section.key, "heading": section.heading, "entries": entries_json })
        })
        .collect();

    Value::Array(sections_json)
}

#[cfg(test)]
mod tests {
    use crate::release_notes::test_support::{charts_item_and_pr, grouped, item, pr};

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
        let sections = grouped(&[p], &[], &items, &HashMap::new());
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
        let sections = grouped(&[p], &[], &[charts_item], &files);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].key, "spec:docs/specs/charts.md");
        assert_eq!(sections[0].heading, "Charts");
    }

    #[test]
    fn group_orders_specs_and_entries_by_the_backlog_not_by_merge_time() {
        let items = vec![
            item("1", "Zebra", ItemStatus::OnMain, None, &["docs/specs/zebra.md"], None),
            item("2", "Apple", ItemStatus::OnMain, None, &["docs/specs/apple.md"], None),
            item("3", "Loose", ItemStatus::OnMain, None, &[], None),
        ];
        let prs = [
            pr(
                1,
                "Apple PR",
                "https://github.com/x/y/pull/1",
                "retaskable/2-apple",
                "2026-09-24T00:00:00Z",
            ),
            pr(
                2,
                "Stray",
                "https://github.com/x/y/pull/2",
                "someone/stray",
                "2026-09-25T00:00:00Z",
            ),
            pr(
                3,
                "Loose PR",
                "https://github.com/x/y/pull/3",
                "retaskable/3-loose",
                "2026-09-26T00:00:00Z",
            ),
            pr(
                4,
                "Zebra PR",
                "https://github.com/x/y/pull/4",
                "retaskable/1-zebra",
                "2026-09-27T00:00:00Z",
            ),
        ];
        let sections = grouped(&prs, &[], &items, &HashMap::new());
        let keys: Vec<&str> = sections.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["spec:docs/specs/zebra.md", "spec:docs/specs/apple.md", "other"]);
        let other: Vec<&str> = sections[2].entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(other, ["item:3", "pr:2"]);
    }

    #[test]
    fn group_falls_back_to_an_items_pr_titles_and_a_chores_own_title() {
        let items = vec![
            item("1", "Charts", ItemStatus::OnMain, None, &[], None),
            item("2", "Bump deps", ItemStatus::OnMain, None, &[], Some("chores")),
        ];
        let prs = [
            pr(
                1,
                "Add charts",
                "https://github.com/x/y/pull/1",
                "retaskable/1-charts",
                "2026-09-24T00:00:00Z",
            ),
            pr(
                2,
                "More charts",
                "https://github.com/x/y/pull/2",
                "retaskable/1-charts",
                "2026-09-25T00:00:00Z",
            ),
            pr(
                3,
                "Bump serde",
                "https://github.com/x/y/pull/3",
                "retaskable/2-bump",
                "2026-09-26T00:00:00Z",
            ),
        ];
        let sections = grouped(&prs, &[], &items, &HashMap::new());
        let chores = sections.iter().find(|s| s.key == "chores").unwrap();
        assert_eq!(chores.entries[0].fallback, "Bump deps");
        assert!(chores.entries[0].item.is_none());
        let other = sections.iter().find(|s| s.key == "other").unwrap();
        assert_eq!(other.entries[0].fallback, "Add charts; More charts");
    }

    #[test]
    fn spec_title_keeps_a_dash_spec_with_no_space_before_spec() {
        assert_eq!(spec_title("docs/specs/x.md", Some("# Charts -Spec\n")), "Charts -Spec");
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
        let sections = grouped(&[p1, p2], &[], &items, &HashMap::new());
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
        let sections = grouped(&[p], &[], &[], &HashMap::new());
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
        let sections = grouped(&[], &[commit], &[], &HashMap::new());
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
        let sections = grouped(&[p], &[commit], &[], &HashMap::new());
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].entries.len(), 1);
        assert_eq!(sections[0].entries[0].key, "pr:1");
    }

    #[test]
    fn group_drops_empty_sections() {
        let sections = grouped(&[], &[], &[], &HashMap::new());
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
        let sections = grouped(&[p1, p2, p3], &[], &items, &HashMap::new());
        let c = counts(&sections, 3);
        assert_eq!(c.prs, 3);
        assert_eq!(c.items, 1);
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
        let sections = grouped(&[p], &[], &items, &HashMap::new());
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
        let sections = grouped(&[p], &[], &[charts_item], &HashMap::new());
        let f = facts(&sections, 120);
        let entry = &f[0]["entries"][0];
        assert_eq!(entry["item"], "1 Charts");
        assert_eq!(entry["status"], "done");
        assert_eq!(entry["prs"][0]["title"], "Add charts");
        assert_eq!(entry["title"], Value::Null);
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
        let sections = grouped(&prs, &[], &items, &HashMap::new());
        let f = facts(&sections, 2);
        let total_title_only: usize = f
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

    // spec_links

    #[test]
    fn spec_links_link_each_markdown_ref_once_at_the_base_branch() {
        let refs: Vec<String> = ["docs/specs/agent/09 web.md", "docs/specs/agent/09 web.md", "src/x.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            spec_links(&refs, "x/y", "main"),
            [SpecLink {
                label: "09 web.md".to_string(),
                url: "https://github.com/x/y/blob/main/docs/specs/agent/09%20web.md".to_string(),
            }]
        );
    }

    #[test]
    fn an_items_facts_name_its_specs_by_file_name() {
        let (charts_item, charts_pr) = charts_item_and_pr();
        let sections = grouped(&[charts_pr], &[], &[charts_item], &HashMap::new());
        assert_eq!(sections[0].entries[0].specs[0].label, "charts.md");
        let f = facts(&sections, DEFAULT_FULL_ENTRIES);
        assert_eq!(f[0]["entries"][0]["specs"], json!(["charts.md"]));
    }

    // spec drafts

    fn draft_pr(files: &[&str]) -> MergedPr {
        let mut p = pr(
            4,
            "Spec the charts",
            "https://github.com/x/y/pull/4",
            "retaskable/draft-1",
            "2026-09-25T00:00:00Z",
        );
        p.files = files.iter().map(|s| s.to_string()).collect();
        p
    }

    fn with_draft(prs: &[MergedPr], items: &[Item]) -> Vec<Section> {
        let drafts = [Draft {
            id: "draft-1".to_string(),
            titles: vec!["Charts".to_string(), "Legends".to_string()],
            pr_url: Some("https://github.com/x/y/pull/4".to_string()),
        }];
        group(&GroupInput {
            prs,
            commits: &[],
            items,
            drafts: &drafts,
            files: &HashMap::new(),
            github_repo: "x/y",
            base_branch: "main",
        })
    }

    #[test]
    fn a_drafts_pr_goes_under_the_spec_it_adds_as_specced() {
        let (charts_item, charts_pr) = charts_item_and_pr();
        let sections = with_draft(
            &[charts_pr, draft_pr(&["src/a.rs", "docs/specs/charts.md"])],
            &[charts_item],
        );
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].key, "spec:docs/specs/charts.md");
        let draft = &sections[0].entries[1];
        assert_eq!(draft.key, "draft:draft-1");
        assert_eq!(draft.fallback, "Specced: Charts; Legends");
        assert_eq!(draft.specs[0].label, "charts.md");
    }

    #[test]
    fn readers_and_the_model_never_see_a_draft() {
        let sections = with_draft(&[draft_pr(&["docs/specs/charts.md"])], &[]);
        assert_eq!(sections[0].entries[0].key, "draft:draft-1");
        assert!(for_users(&sections).is_empty());
        assert_eq!(facts(&sections, DEFAULT_FULL_ENTRIES), json!([]));
    }

    #[test]
    fn a_drafts_pr_that_changed_no_spec_is_another_change() {
        let sections = with_draft(&[draft_pr(&["src/a.rs"])], &[]);
        assert_eq!(sections[0].key, "other");
        assert_eq!(sections[0].entries[0].key, "pr:4");
    }
}
