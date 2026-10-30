//! A spec's Items header (Retaskable.com's docs/specs/backlog.md section 5, `shared/specItems.ts`): read only to
//! seed a store that lacks an item, since the store's item is the source of truth. Each spec's opening, before its
//! first `## ` heading, may hold an `Items:` line followed by one line per item:
//!
//! ```text
//! - **<id>**: <title>. Depends on: <none | id, id>.
//! - **<id>**: <title>. Depends on: none. Specs: [cloud.md](cloud.md) section 4.
//! ```
//!
//! Plain code reads them; [`check_spec_items`] lists a duplicate id, a dependency that names no item, a cycle or a
//! malformed line.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// Where a repo's specs and their Items headers live (`SPECS_DIR` in `shared/specsDir.ts`).
pub const SPECS_DIR: &str = "docs/specs";

/// The longest milestone the store keeps (backlog.md section 6.1).
const MILESTONE_MAX: usize = 60;

static LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^- \*\*([^*]+)\*\*: (.+?)\. Depends on: ([^.]+)\.(?: (Specs: .*))?$").expect("a valid regex")
});
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(?:-[a-z0-9]+)*$").expect("a valid regex"));
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[^\]]*\]\(([^)\s]+)\)").expect("a valid regex"));
static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("a valid regex"));

/// One item a header names, as `upsert_from_specs` sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecItem {
    pub id: String,
    pub title: String,
    pub depends_on: Vec<String>,
    /// The spec whose header names it, then the files its line links.
    pub spec_refs: Vec<String>,
    /// From its spec's `Kind:` line (card-kind-colours.md section 3.1); none for a write-up or no line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// From its spec's `Theme:` line (the first theme when it lists several); none with no line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// The spec that holds its header line, and the line's number (1-based).
    #[serde(skip)]
    pub spec: String,
    #[serde(skip)]
    pub line: usize,
}

/// Something wrong in a header, at a spec's line (1-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemsProblem {
    pub spec: String,
    pub line: usize,
    pub message: String,
}

impl ItemsProblem {
    pub fn is_cycle(&self) -> bool {
        self.message.starts_with("A cycle")
    }
}

/// A readable slug: lowercase words joined by hyphens, never a bare number.
pub fn is_item_id(id: &str) -> bool {
    SLUG.is_match(id) && !id.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// "docs/specs/build-order.md" + "../premise.md" is "docs/premise.md".
pub fn resolve_path(from: &str, href: &str) -> String {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for seg in href.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "" | "." => {}
            seg => parts.push(seg),
        }
    }
    parts.join("/")
}

/// A link's file name, percent-decoded; as written when it isn't valid percent-encoding.
fn decode_file(file: &str) -> String {
    let bytes = file.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = file.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(b) = hex else { return file.to_string() };
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| file.to_string())
}

/// The other `.md` files `content` (a file at `from`) links to, in order (`readLinks` in `shared/specLinks.ts`, less
/// the sections).
pub fn read_links(from: &str, content: &str) -> Vec<String> {
    LINK.captures_iter(content)
        .filter_map(|m| {
            let href = &m[1];
            if SCHEME.is_match(href) || href.starts_with('#') {
                return None;
            }
            let file = href.split('#').next().unwrap_or_default();
            if !file.to_lowercase().ends_with(".md") {
                return None;
            }
            let to = resolve_path(from, &decode_file(file));
            (to != from).then_some(to)
        })
        .collect()
}

/// Where a spec's opening ends: the index of its first `## ` line, or the line count.
fn opening_end(lines: &[&str]) -> usize {
    lines.iter().position(|l| l.starts_with("## ")).unwrap_or(lines.len())
}

/// An opening's `Kind:` line as its items' kind: a new spec's are features, a change's tweaks; a write-up's none.
fn spec_kind(opening: &[&str]) -> Option<String> {
    let word = opening.iter().find_map(|l| {
        let rest = l.trim().strip_prefix("Kind:")?.trim();
        (!rest.is_empty() && !rest.contains(char::is_whitespace)).then(|| rest.to_lowercase())
    })?;
    let kind = match word.as_str() {
        "new" | "feature" => "feature",
        "change" | "tweak" => "tweak",
        "fix" => "fix",
        _ => return None,
    };
    Some(kind.to_string())
}

/// An opening's `Theme:` line as its items' milestone: the first theme, when it lists several.
fn spec_theme(opening: &[&str]) -> Option<String> {
    let line = opening
        .iter()
        .find_map(|l| l.trim().strip_prefix("Theme:").map(str::trim).filter(|t| !t.is_empty()))?;
    let theme: String = line
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(MILESTONE_MAX)
        .collect();
    (!theme.is_empty()).then_some(theme)
}

/// The Items header of one spec (`spec` a repo path like `docs/specs/backlog.md`).
pub fn read_spec_items(spec: &str, content: &str) -> (Vec<SpecItem>, Vec<ItemsProblem>) {
    let lines: Vec<&str> = content.lines().collect();
    let end = opening_end(&lines);
    let kind = spec_kind(&lines[..end]);
    let milestone = spec_theme(&lines[..end]);
    let mut items = Vec::new();
    let mut problems = Vec::new();
    let mut problem = |i: usize, message: String| {
        problems.push(ItemsProblem {
            spec: spec.to_string(),
            line: i + 1,
            message,
        })
    };
    let mut seen = false;
    let mut i = 0;
    while i < end {
        if lines[i].trim() != "Items:" {
            i += 1;
            continue;
        }
        if seen {
            problem(i, "A second Items: block".into());
        }
        seen = true;
        let mut j = i + 1;
        while j < end && lines[j].starts_with("- ") {
            match read_line(spec, lines[j]) {
                Ok((id, title, depends_on, spec_refs)) => items.push(SpecItem {
                    id,
                    title,
                    depends_on,
                    spec_refs,
                    kind: kind.clone(),
                    milestone: milestone.clone(),
                    spec: spec.to_string(),
                    line: j + 1,
                }),
                Err(message) => problem(j, message),
            }
            j += 1;
        }
        if j == i + 1 {
            problem(i, "Items: has no item lines under it".into());
        }
        i = j;
    }
    (items, problems)
}

type Line = (String, String, Vec<String>, Vec<String>);

fn read_line(spec: &str, text: &str) -> Result<Line, String> {
    let Some(m) = LINE.captures(text.trim_end()) else {
        let cut: String = text.chars().take(80).collect();
        return Err(format!(
            "Not an item line (\"- **id**: Title. Depends on: none.\"): {cut}"
        ));
    };
    let id = m[1].to_string();
    if !is_item_id(&id) {
        return Err(format!(
            "{id} isn't a readable slug (lowercase words joined by hyphens)"
        ));
    }
    let deps = m[3].trim();
    let depends_on: Vec<String> = if deps == "none" {
        Vec::new()
    } else {
        deps.split(',').map(|d| d.trim().to_string()).collect()
    };
    let bad: Vec<&str> = depends_on
        .iter()
        .map(String::as_str)
        .filter(|d| !is_item_id(d))
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "{id} depends on {}, which isn't a readable slug",
            bad.join(", ")
        ));
    }
    let mut spec_refs = vec![spec.to_string()];
    for to in read_links(spec, text) {
        if !spec_refs.contains(&to) {
            spec_refs.push(to);
        }
    }
    Ok((id, m[2].trim().to_string(), depends_on, spec_refs))
}

/// Every spec's Items header together (`files` as (path, content)): each id once (the first stands), and what's
/// wrong across them. `known`: ids the backlog already has from elsewhere, which a header may depend on though no
/// spec names them.
pub fn check_spec_items(files: &[(String, String)], known: &BTreeSet<String>) -> (Vec<SpecItem>, Vec<ItemsProblem>) {
    let mut all = Vec::new();
    let mut problems = Vec::new();
    for (path, content) in files {
        let (items, found) = read_spec_items(path, content);
        all.extend(items);
        problems.extend(found);
    }
    let mut items: Vec<SpecItem> = Vec::new();
    let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
    for item in all {
        if let Some(&k) = by_id.get(&item.id) {
            let first = &items[k];
            problems.push(ItemsProblem {
                spec: item.spec.clone(),
                line: item.line,
                message: format!(
                    "{} is named twice (first in {} line {})",
                    item.id, first.spec, first.line
                ),
            });
        } else {
            by_id.insert(item.id.clone(), items.len());
            items.push(item);
        }
    }
    let problem = |item: &SpecItem, message: String| ItemsProblem {
        spec: item.spec.clone(),
        line: item.line,
        message,
    };
    for item in &items {
        for dep in &item.depends_on {
            if *dep == item.id {
                problems.push(problem(item, format!("{} depends on itself", item.id)));
            } else if !by_id.contains_key(dep) && !known.contains(dep) {
                problems.push(problem(
                    item,
                    format!("{} depends on {dep}, which no spec names", item.id),
                ));
            }
        }
    }
    // Cycles: a depth-first walk, reporting each cycle once, at the item it was found from.
    let mut done = BTreeSet::new();
    for item in &items {
        walk(&item.id, &mut Vec::new(), &items, &by_id, &mut done, &mut problems);
    }
    (items, problems)
}

fn walk(
    id: &str,
    path: &mut Vec<String>,
    items: &[SpecItem],
    by_id: &BTreeMap<String, usize>,
    done: &mut BTreeSet<String>,
    problems: &mut Vec<ItemsProblem>,
) {
    if done.contains(id) {
        return;
    }
    if let Some(at) = path.iter().position(|p| p == id) {
        if path.len() - at > 1 {
            let item = &items[by_id[id]];
            let mut cycle = path[at..].to_vec();
            cycle.push(id.to_string());
            problems.push(ItemsProblem {
                spec: item.spec.clone(),
                line: item.line,
                message: format!("A cycle: {}", cycle.join(" → ")),
            });
        }
        return;
    }
    path.push(id.to_string());
    for dep in &items[by_id[id]].depends_on {
        if by_id.contains_key(dep) {
            walk(dep, path, items, by_id, done, problems);
        }
    }
    path.pop();
    done.insert(id.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(body: &str) -> (String, String) {
        ("docs/specs/cloud.md".to_string(), body.to_string())
    }

    #[test]
    fn reads_an_items_header_with_its_kind_theme_and_links() {
        let body = "# Cloud\n\nKind: new\nTheme: Hosting, Sync\n\nItems:\n\
            - **cloud-link**: Link to the cloud. Depends on: none. Specs: [cloud.md](cloud.md) section 4, [x](../premise.md#a).\n\
            - **cloud-sync**: Sync it. Depends on: cloud-link.\n\n## 1. Body\n\nItems:\n- **later**: Not read. Depends on: none.\n";
        let (items, problems) = read_spec_items("docs/specs/cloud.md", body);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(items.len(), 2, "only the opening's header counts");
        assert_eq!(items[0].id, "cloud-link");
        assert_eq!(items[0].title, "Link to the cloud");
        assert_eq!(items[0].spec_refs, ["docs/specs/cloud.md", "docs/premise.md"]);
        assert_eq!(items[0].kind.as_deref(), Some("feature"));
        assert_eq!(items[0].milestone.as_deref(), Some("Hosting"));
        assert_eq!(
            (items[1].depends_on.clone(), items[1].line),
            (vec!["cloud-link".to_string()], 8)
        );
        assert_eq!(
            serde_json::to_value(&items[1]).unwrap(),
            serde_json::json!({
                "id": "cloud-sync", "title": "Sync it", "dependsOn": ["cloud-link"],
                "specRefs": ["docs/specs/cloud.md"], "kind": "feature", "milestone": "Hosting"
            })
        );
    }

    #[test]
    fn a_bad_line_a_bare_number_and_an_empty_header_are_problems() {
        let body = "Items:\n- **42**: A number. Depends on: none.\n- not an item\n\nItems:\n";
        let (items, problems) = read_spec_items("s.md", body);
        assert!(items.is_empty());
        let messages: Vec<_> = problems.iter().map(|p| (p.line, p.message.as_str())).collect();
        assert!(messages[0].1.contains("isn't a readable slug"), "{messages:?}");
        assert!(messages[1].1.starts_with("Not an item line"));
        assert_eq!(messages[2], (5, "A second Items: block"));
        assert_eq!(messages[3], (5, "Items: has no item lines under it"));
    }

    #[test]
    fn checking_finds_twice_named_unknown_self_and_cycles_and_keeps_the_first() {
        let files = [
            spec(
                "Items:\n- **a**: A. Depends on: b.\n- **b**: B. Depends on: a.\n- **c**: C. Depends on: c, gone, old.\n",
            ),
            (
                "docs/specs/other.md".into(),
                "Items:\n- **a**: Again. Depends on: none.\n".into(),
            ),
        ];
        let known = BTreeSet::from(["old".to_string()]);
        let (items, problems) = check_spec_items(&files, &known);
        assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(items[0].title, "A", "the first stands");
        let messages: Vec<_> = problems.iter().map(|p| p.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "a is named twice (first in docs/specs/cloud.md line 2)",
                "c depends on itself",
                "c depends on gone, which no spec names",
                "A cycle: a → b → a",
            ]
        );
        assert!(problems[3].is_cycle());
    }

    #[test]
    fn links_resolve_against_the_spec_and_skip_urls_anchors_and_other_files() {
        let text = "[a](../x.md) [b](https://e.com/y.md) [c](#top) [d](pic.png) [e](sub/My%20Spec.md#s) [f](cloud.md)";
        assert_eq!(
            read_links("docs/specs/cloud.md", text),
            ["docs/x.md", "docs/specs/sub/My Spec.md"]
        );
        assert_eq!(decode_file("a%zz.md"), "a%zz.md");
    }
}
