//! Deprecated specs (deprecate-feature.md section 3.1), the pure part of the TypeScript agent's
//! `deprecations.ts`: a spec whose header says `Deprecated: <day>. Remove after: <day>. Why: <why>.`
//! gets one `remove-<name>` item once the project's day is after that date. No I/O here.
use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::NaiveDate;
use regex::Regex;
use serde_json::{Value, json};

use crate::spec_items::is_item_id;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deprecation {
    /// The spec's path in the repo, e.g. `docs/specs/mobile.md`.
    pub spec: String,
    /// Its file name without `.md`: what the removal item is named after.
    pub name: String,
    /// Its `# ` heading, without " — Spec".
    pub title: String,
    pub since: String,
    pub remove_after: String,
    pub why: Option<String>,
}

/// A `Deprecated:` line that couldn't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecationProblem {
    pub spec: String,
    pub line: String,
    pub message: String,
}

static DAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());
static LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Deprecated:\s*(\S+?)\.?\s+Remove after:\s*(\S+?)\.?(?:\s+Why:\s*(.*?))?\s*$").unwrap()
});
static SPEC_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s+[—–-]+\s*Spec\s*$").unwrap());

/// A real `yyyy-mm-dd` day: 2026-02-30 isn't one.
fn real_day(s: &str) -> bool {
    DAY.is_match(s) && NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// The file name without `.md`; a Windows path reads the same as a POSIX one.
fn spec_name(spec: &str) -> String {
    let normal = spec.replace('\\', "/");
    let file = std::path::Path::new(&normal)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    file.strip_suffix(".md").unwrap_or(file).to_string()
}

/// One spec's `Deprecated:` line, from its header (above its first `## `): `None` without one.
pub fn parse_deprecation(spec: &str, content: &str) -> Option<Result<Deprecation, DeprecationProblem>> {
    let lines: Vec<&str> = content.lines().collect();
    let end = lines.iter().position(|l| l.starts_with("## ")).unwrap_or(lines.len());
    let header = &lines[..end];
    let line = header.iter().map(|l| l.trim()).find(|l| l.starts_with("Deprecated:"))?;
    let problem = |message: String| {
        Some(Err(DeprecationProblem {
            spec: spec.to_string(),
            line: line.to_string(),
            message,
        }))
    };
    let Some(m) = LINE.captures(line) else {
        return problem("it should read `Deprecated: yyyy-mm-dd. Remove after: yyyy-mm-dd. Why: …`".into());
    };
    let (since, remove_after) = (&m[1], &m[2]);
    for day in [since, remove_after] {
        if !real_day(day) {
            return problem(format!("{day} isn't a date (yyyy-mm-dd)"));
        }
    }
    let name = spec_name(spec);
    if !is_item_id(&format!("remove-{name}")) {
        return problem(format!("remove-{name} can't be an item's id"));
    }
    let title = match header.iter().find(|l| l.starts_with("# ")) {
        Some(h) => SPEC_SUFFIX.replace(&h[2..], "").trim().to_string(),
        None => name.clone(),
    };
    let why = m.get(3).and_then(|w| {
        let w = w.as_str().trim();
        let w = w.strip_suffix('.').unwrap_or(w);
        (!w.is_empty()).then(|| w.to_string())
    });
    Some(Ok(Deprecation {
        spec: spec.to_string(),
        name,
        title,
        since: since.to_string(),
        remove_after: remove_after.to_string(),
        why,
    }))
}

/// Every deprecated spec among `files` (path, content), and the lines that couldn't be read.
pub fn read_deprecations(files: &[(String, String)]) -> (Vec<Deprecation>, Vec<DeprecationProblem>) {
    let (mut deprecations, mut problems) = (Vec::new(), Vec::new());
    for (path, content) in files {
        match parse_deprecation(path, content) {
            Some(Ok(d)) => deprecations.push(d),
            Some(Err(p)) => problems.push(p),
            None => {}
        }
    }
    (deprecations, problems)
}

pub fn removal_id(d: &Deprecation) -> String {
    format!("remove-{}", d.name)
}

/// Due: the day is after `Remove after`, and no `remove-<spec>` item exists, in any state.
pub fn removal_due(d: &Deprecation, today: &str, existing: &HashSet<String>) -> bool {
    today > d.remove_after.as_str() && !existing.contains(&removal_id(d))
}

/// The removal item, as the backlog's `add` takes it.
pub fn removal_item(d: &Deprecation) -> Value {
    let mut remaining = format!(
        "Delete everything [{spec}]({spec}) describes: its code, tests, UI, config, and the commands, routes or schema it alone uses. \
         Keep anything another live spec still needs, and say so. Delete the spec file, and fix or remove every link to it in other specs and the README. \
         A migration that drops tables or columns goes in its own slice, after the code that uses them is gone.",
        spec = d.spec
    );
    if let Some(why) = &d.why {
        remaining.push_str(&format!(" Why it's going: {why}."));
    }
    json!({
        "id": removal_id(d),
        "title": format!("Remove {}", d.title),
        "specRefs": [d.spec],
        "remaining": remaining,
    })
}

/// The store's lock that makes one agent add a spec's removal item (backlog.md section 15).
pub fn deprecation_lock(d: &Deprecation) -> String {
    let lock: String = format!("deprecation-{}", d.name).chars().take(60).collect();
    lock.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(name: &str) -> Deprecation {
        Deprecation {
            spec: format!("docs/specs/{name}.md"),
            name: name.into(),
            title: "Mobile".into(),
            since: "2026-01-01".into(),
            remove_after: "2026-02-01".into(),
            why: None,
        }
    }

    fn ok(spec: &str, content: &str) -> Deprecation {
        parse_deprecation(spec, content).unwrap().unwrap()
    }

    fn bad(spec: &str, content: &str) -> DeprecationProblem {
        parse_deprecation(spec, content).unwrap().unwrap_err()
    }

    #[test]
    fn parses_with_and_without_why() {
        let c =
            "# Mobile — Spec\n\nDeprecated: 2026-01-01. Remove after: 2026-02-01. Why: Nobody uses it.\n\n## 1. A\n";
        let d = ok("docs/specs/mobile.md", c);
        assert_eq!(
            d,
            Deprecation {
                why: Some("Nobody uses it".into()),
                ..dep("mobile")
            }
        );
        let d = ok(
            "docs/specs/mobile.md",
            "# Mobile\nDeprecated: 2026-01-01. Remove after: 2026-02-01.\n",
        );
        assert_eq!(d.why, None);
        assert_eq!(d.title, "Mobile");
        assert_eq!(d.remove_after, "2026-02-01");
    }

    #[test]
    fn name_comes_from_the_file_stem_on_any_path() {
        let c = "Deprecated: 2026-01-01. Remove after: 2026-02-01\n";
        assert_eq!(ok(r"docs\specs\mobile.md", c).name, "mobile");
        assert_eq!(ok("mobile.md", c).title, "mobile");
    }

    #[test]
    fn ignores_lines_below_the_first_section() {
        let c = "# T\n\n## 1. A\nDeprecated: 2026-01-01. Remove after: 2026-02-01.\n";
        assert!(parse_deprecation("docs/specs/a.md", c).is_none());
        assert!(parse_deprecation("docs/specs/a.md", "# T\nnothing\n").is_none());
    }

    #[test]
    fn problems() {
        let p = bad("docs/specs/a.md", "Deprecated: soon.\n");
        assert!(p.message.starts_with("it should read"));
        assert_eq!(p.line, "Deprecated: soon.");
        let p = bad("docs/specs/a.md", "Deprecated: 2026-02-30. Remove after: 2026-03-01.\n");
        assert_eq!(p.message, "2026-02-30 isn't a date (yyyy-mm-dd)");
        let p = bad("docs/specs/a.md", "Deprecated: 2026-01-01. Remove after: 2026-3-1.\n");
        assert_eq!(p.message, "2026-3-1 isn't a date (yyyy-mm-dd)");
        let p = bad(
            "docs/specs/Bad Name.md",
            "Deprecated: 2026-01-01. Remove after: 2026-02-01.\n",
        );
        assert_eq!(p.message, "remove-Bad Name can't be an item's id");
    }

    #[test]
    fn reads_many() {
        let files = vec![
            (
                "docs/specs/a.md".to_string(),
                "Deprecated: 2026-01-01. Remove after: 2026-02-01.".to_string(),
            ),
            ("docs/specs/b.md".to_string(), "Deprecated: x".to_string()),
            ("docs/specs/c.md".to_string(), "# C".to_string()),
        ];
        let (d, p) = read_deprecations(&files);
        assert_eq!((d.len(), p.len()), (1, 1));
        assert_eq!(p[0].spec, "docs/specs/b.md");
    }

    #[test]
    fn removal_due_needs_a_later_day_and_no_item() {
        let d = dep("mobile");
        let none = HashSet::new();
        assert!(!removal_due(&d, "2026-01-31", &none));
        assert!(!removal_due(&d, "2026-02-01", &none));
        assert!(removal_due(&d, "2026-02-02", &none));
        let existing: HashSet<String> = ["remove-mobile".to_string()].into();
        assert!(!removal_due(&d, "2026-02-02", &existing));
    }

    #[test]
    fn removal_item_text() {
        let mut d = dep("mobile");
        let item = removal_item(&d);
        assert_eq!(item["id"], "remove-mobile");
        assert_eq!(item["title"], "Remove Mobile");
        assert_eq!(item["specRefs"], json!(["docs/specs/mobile.md"]));
        let base = "Delete everything [docs/specs/mobile.md](docs/specs/mobile.md) describes: its code, tests, UI, config, and the commands, routes or schema it alone uses. Keep anything another live spec still needs, and say so. Delete the spec file, and fix or remove every link to it in other specs and the README. A migration that drops tables or columns goes in its own slice, after the code that uses them is gone.";
        assert_eq!(item["remaining"], base);
        d.why = Some("Nobody uses it".into());
        assert_eq!(
            removal_item(&d)["remaining"],
            format!("{base} Why it's going: Nobody uses it.")
        );
    }

    #[test]
    fn lock_is_cut_to_sixty_without_a_trailing_dash() {
        assert_eq!(deprecation_lock(&dep("mobile")), "deprecation-mobile");
        let long = format!("{}-tail", "a".repeat(47));
        let lock = deprecation_lock(&dep(&long));
        assert_eq!(lock.len(), 59);
        assert!(!lock.ends_with('-'));
        assert_eq!(deprecation_lock(&dep(&"a".repeat(80))).len(), 60);
    }
}
