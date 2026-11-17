//! Whether an idea's plan needs a design review (TS `shared/planProgressLines.ts` `needsDesignOf`,
//! `shared/planPreview.ts`; design-view.md section 11, design-view/14): a feature with a picture safe to draw, that
//! did not itself come from a design. Plain code, no model: a fix, a tweak or a plan with no picture needs none.
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::drafts::files::DraftFile;
use crate::spec_items::read_spec_items;

/// What **Approve** is refused with while the plan waits for its design review (`approveDraft`).
pub const NEEDS_DESIGN_FIRST: &str = "This plan needs a design review first: review the designs, or skip the design.";

const MAX_BYTES: usize = 100 * 1024;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

/// The title of a plan made from a design: it is never sent to a design review again.
pub fn is_design_title(title: &str) -> bool {
    title.trim().get(..7).is_some_and(|t| t.eq_ignore_ascii_case("Design:"))
}

/// The plan's added or edited files that are previews (`<name>.preview.svg` or `<name>.preview-<n>.svg`), in path order.
pub fn preview_files(files: &[DraftFile]) -> Vec<&DraftFile> {
    static PREVIEW: OnceLock<Regex> = OnceLock::new();
    let preview = re(&PREVIEW, r"(?i)\.preview(-\d+)?\.svg$");
    let mut found: Vec<&DraftFile> = files
        .iter()
        .filter(|f| !f.deleted && preview.is_match(&f.path))
        .collect();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Why a preview isn't drawn (plan-preview.md section 2, "Safe to draw"), or `None` when it can be.
pub fn preview_problem(content: &str) -> Option<&'static str> {
    static HEAD: OnceLock<[Regex; 3]> = OnceLock::new();
    static TESTS: OnceLock<[Regex; 4]> = OnceLock::new();
    static HREF: OnceLock<Regex> = OnceLock::new();
    if content.len() > MAX_BYTES {
        return Some("it's over 100 KB");
    }
    let [xml, comment, doctype] = HEAD.get_or_init(|| {
        [
            Regex::new(r"<\?xml[\s\S]*?\?>").unwrap(),
            Regex::new(r"<!--[\s\S]*?-->").unwrap(),
            Regex::new(r"(?i)<!DOCTYPE[^>]*>").unwrap(),
        ]
    });
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let body = xml.replace_all(body, "");
    let body = comment.replace_all(&body, "");
    let body = doctype.replace_all(&body, "");
    let [svg, script, foreign, handler] = TESTS.get_or_init(|| {
        [
            Regex::new(r"(?i)^<svg[\s>]").unwrap(),
            Regex::new(r"(?i)<script[\s>/]").unwrap(),
            Regex::new(r"(?i)<foreignObject[\s>/]").unwrap(),
            Regex::new(r#"(?i)<[^>]*[\s/"']on[a-z]+\s*="#).unwrap(),
        ]
    });
    if !svg.is_match(body.trim()) {
        return Some("it isn't an SVG");
    }
    if script.is_match(content) {
        return Some("it has a script");
    }
    if foreign.is_match(content) {
        return Some("it has embedded HTML");
    }
    if handler.is_match(content) {
        return Some("it has an event handler");
    }
    let href = re(&HREF, r#"(?i)\s(?:xlink:)?href\s*=\s*("([^"]*)"|'([^']*)'|([^\s>]+))"#);
    for m in href.captures_iter(content) {
        let target = m.get(2).or(m.get(3)).or(m.get(4)).map_or("", |t| t.as_str()).trim();
        if !target.starts_with('#') {
            return Some("it links outside itself");
        }
    }
    None
}

/// The kinds of the items a plan adds (`ids`), read from the `Kind:` line of the specs it changed: one for each, each a
/// tweak or a feature, or none when any is a write-up, has no line or isn't found (`entryIds`' `kindsOk`).
pub fn item_kinds(files: &[DraftFile], ids: &[String]) -> Option<Vec<String>> {
    if ids.is_empty() {
        return None;
    }
    let mut found: Vec<(String, Option<String>)> = Vec::new();
    for f in files.iter().filter(|f| !f.deleted && f.path.ends_with(".md")) {
        for item in read_spec_items(&f.path, &f.content).0 {
            found.push((item.id, item.kind));
        }
    }
    ids.iter()
        .map(|id| {
            let kind = found.iter().find(|(i, _)| i == id)?.1.clone()?;
            matches!(kind.as_str(), "tweak" | "feature").then_some(kind)
        })
        .collect()
}

/// Whether an idea's plan needs a design approval: a feature with a picture safe to draw, not a plan made from a
/// design. A fix, a tweak or a plan with no picture needs none.
pub fn needs_design_of(title: &str, item_kinds: Option<&[String]>, files: &[DraftFile]) -> bool {
    if is_design_title(title) || !item_kinds.is_some_and(|k| k.iter().any(|k| k == "feature")) {
        return false;
    }
    preview_files(files)
        .iter()
        .any(|f| preview_problem(&f.content).is_none())
}

/// Whether the store's copy of a draft says it waits for its design review (`needsDesign`).
pub fn design_pending(draft: &Map<String, Value>) -> bool {
    draft.get("needsDesign").and_then(Value::as_bool) == Some(true)
}

/// The reason the designs weren't drawn, when the store's copy says (`designNote`).
pub fn design_note(draft: &Map<String, Value>) -> Option<&str> {
    draft
        .get("designNote")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, content: &str) -> DraftFile {
        DraftFile {
            path: path.into(),
            old_path: None,
            is_new: false,
            content: content.into(),
            patch: None,
            deleted: false,
            note: None,
        }
    }

    const PLAIN: &str = r##"<?xml version="1.0"?>
<!-- a sketch -->
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 390 300"><defs><g id="b"/></defs><rect width="390" height="300" fill="#eee"/><use xlink:href="#b"/><text x="20" y="40">Approve plan</text></svg>"##;

    #[test]
    fn previews_are_the_added_or_edited_sketches_in_path_order() {
        let mut gone = file("docs/specs/c.preview.svg", "");
        gone.deleted = true;
        let files = [
            file("docs/specs/b.preview.svg", ""),
            file("docs/specs/a.md", ""),
            file("docs/specs/a.preview-2.svg", ""),
            file("docs/specs/a.preview-1.svg", ""),
            file("docs/specs/logo.svg", ""),
            gone,
            file("docs/specs/d.preview.svg.md", ""),
        ];
        let paths: Vec<&str> = preview_files(&files).iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "docs/specs/a.preview-1.svg",
                "docs/specs/a.preview-2.svg",
                "docs/specs/b.preview.svg"
            ]
        );
        assert!(preview_files(&[file("docs/specs/a.md", "")]).is_empty());
    }

    #[test]
    fn a_plain_sketch_passes_and_each_refusal_says_why() {
        assert_eq!(preview_problem(PLAIN), None);
        let big = format!("<svg viewBox=\"0 0 1 1\">{}</svg>", "x".repeat(100 * 1024));
        assert_eq!(preview_problem(&big), Some("it's over 100 KB"));
        assert_eq!(preview_problem("<html><body/></html>"), Some("it isn't an SVG"));
        assert_eq!(preview_problem(""), Some("it isn't an SVG"));
        assert_eq!(
            preview_problem("<svg><script>alert(1)</script></svg>"),
            Some("it has a script")
        );
        assert_eq!(
            preview_problem("<svg><foreignObject><div/></foreignObject></svg>"),
            Some("it has embedded HTML")
        );
        assert_eq!(
            preview_problem(r#"<svg><rect onclick="x()"/></svg>"#),
            Some("it has an event handler")
        );
        assert_eq!(
            preview_problem("<svg onload='x()'></svg>"),
            Some("it has an event handler")
        );
        // A browser also splits attributes at "/" and after a closing quote, with no space.
        assert_eq!(
            preview_problem("<svg><rect/onload=x()/></svg>"),
            Some("it has an event handler")
        );
        assert_eq!(
            preview_problem(r#"<svg><rect fill="red"onclick="x()"/></svg>"#),
            Some("it has an event handler")
        );
        assert_eq!(
            preview_problem(r#"<svg><a href="https://example.com"><text>x</text></a></svg>"#),
            Some("it links outside itself")
        );
        assert_eq!(
            preview_problem(r#"<svg><image xlink:href="data:image/png;base64,AA"/></svg>"#),
            Some("it links outside itself")
        );
    }

    fn kinds(k: &[&str]) -> Option<Vec<String>> {
        Some(k.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_feature_with_a_safe_picture_needs_a_design_and_a_fix_a_tweak_or_no_picture_does_not() {
        let svg = file("docs/specs/x.preview.svg", PLAIN);
        let md = file("docs/specs/x.md", "");
        let both = [md.clone(), svg.clone()];
        let needs =
            |title: &str, k: Option<Vec<String>>, files: &[DraftFile]| needs_design_of(title, k.as_deref(), files);
        assert!(needs("Snooze", kinds(&["feature"]), &both));
        assert!(needs("Snooze", kinds(&["tweak", "feature"]), &both));
        assert!(!needs("Snooze", kinds(&["tweak"]), &both));
        assert!(!needs("Snooze", kinds(&["feature"]), std::slice::from_ref(&md)));
        assert!(!needs("Snooze", None, &both));
        assert!(!needs("Design: Snooze", kinds(&["feature"]), &both));
        assert!(!needs("  design: Snooze", kinds(&["feature"]), &both));
        let scripted = file("docs/specs/x.preview.svg", "<svg><script></script></svg>");
        assert!(!needs("Snooze", kinds(&["feature"]), &[md, scripted]));
    }

    #[test]
    fn item_kinds_come_from_the_kind_line_of_the_changed_specs_for_the_items_the_plan_adds() {
        let spec = "# Snooze\n\nKind: new\n\nItems:\n- **snooze**: Snooze an alert. Depends on: none.\n";
        let files = [file("docs/specs/snooze.md", spec)];
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(item_kinds(&files, &ids(&["snooze"])), kinds(&["feature"]));
        assert_eq!(item_kinds(&files, &ids(&["snooze", "other"])), None, "one for each");
        assert_eq!(item_kinds(&files, &[]), None);
        assert_eq!(
            item_kinds(&[file("docs/specs/snooze.md", "# no header\n")], &ids(&["snooze"])),
            None
        );
    }

    #[test]
    fn the_stores_copy_says_pending_and_why() {
        let d = serde_json::json!({"needsDesign": true, "designNote": "Claude is signed out"});
        let d = d.as_object().unwrap();
        assert!(design_pending(d));
        assert_eq!(design_note(d), Some("Claude is signed out"));
        assert!(!design_pending(&Map::new()) && design_note(&Map::new()).is_none());
    }
}
