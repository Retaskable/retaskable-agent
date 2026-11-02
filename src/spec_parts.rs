//! Reads a split spec (docs/specs/agent/06.10-spec-layout.md, "A split spec" and "Reading one"):
//! an index `docs/specs/<name>.md` whose `##` headings link to section files in
//! `docs/specs/<name>/`. Port of `shared/specParts.ts`. Paths in and out are relative to `root`;
//! an unsplit spec is just itself.
use std::path::{Path, PathBuf};

use regex::Regex;

/// The text before the index's first `##` heading, and the section files its `##` sections link to
/// (in order, once each, only those inside the spec's own folder).
fn read_index(index_path: &Path, root: &Path) -> (String, Vec<PathBuf>) {
    let text = std::fs::read_to_string(root.join(index_path)).unwrap_or_default();
    let dir = index_path.parent().unwrap_or(Path::new(""));
    let folder = index_path.file_stem().map(|s| dir.join(s));
    let link = Regex::new(r"\]\(([^)\s#]+\.md)(?:#[^)]*)?\)").unwrap();
    let (mut opening, mut parts) = (String::new(), Vec::new());
    let mut in_sections = false;
    for line in text.split_inclusive('\n') {
        if line.starts_with("## ") {
            in_sections = true;
        }
        if !in_sections {
            opening.push_str(line);
            continue;
        }
        for caps in link.captures_iter(line) {
            let mut path = dir.to_path_buf();
            path.extend(caps[1].split('/').filter(|c| !c.is_empty() && *c != "."));
            if path.parent() == folder.as_deref() && !parts.contains(&path) {
                parts.push(path);
            }
        }
    }
    (opening, parts)
}

/// A split spec's section files in index order (`-2` "(continued)" files after their part); an
/// unsplit spec gives just itself.
pub fn spec_parts(index_path: &Path, root: &Path) -> Vec<PathBuf> {
    let (_, parts) = read_index(index_path, root);
    if parts.is_empty() {
        vec![index_path.to_path_buf()]
    } else {
        parts
    }
}

/// The whole text: an unsplit spec's file, or a split one's opening followed by each part.
pub fn whole_spec(index_path: &Path, root: &Path) -> String {
    let (opening, parts) = read_index(index_path, root);
    if parts.is_empty() {
        return std::fs::read_to_string(root.join(index_path)).unwrap_or_default();
    }
    let mut out = opening;
    for part in parts {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&std::fs::read_to_string(root.join(part)).unwrap_or_default());
    }
    out
}

/// The numbers a section file's name starts with (`06.2-implementer.md` → [6, 2]).
fn file_number(path: &Path) -> Option<Vec<u32>> {
    let name = path.file_name()?.to_str()?;
    let head = name.split('-').next()?;
    head.split('.').map(|n| n.parse().ok()).collect()
}

/// The file holding section `section` (`7`, `6.2`): the unsplit spec itself; else the file named
/// for that number, falling back to the file of the `##` section above a `###` that wasn't split
/// out. None when no file holds it.
pub fn section_file(index_path: &Path, section: &str, root: &Path) -> Option<PathBuf> {
    let (_, parts) = read_index(index_path, root);
    if parts.is_empty() {
        return Some(index_path.to_path_buf());
    }
    let mut wanted: Vec<u32> = section
        .trim()
        .split('.')
        .map(|n| n.parse().ok())
        .collect::<Option<_>>()?;
    loop {
        if let Some(found) = parts.iter().find(|p| file_number(p).as_ref() == Some(&wanted)) {
            return Some(found.clone());
        }
        wanted.pop();
        if wanted.is_empty() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "# Spec\n\nOpening.\n\nThis spec is split.\n\n## 6. Sessions\n\n\
- [s/06-x.md](s/06-x.md)\n- [s/06.2-y.md](s/06.2-y.md): 6.2 Y\n- [s/06-x-2.md](s/06-x-2.md) (continued)\n\n\
## 7. Loop\n\n- [s/07-loop.md](s/07-loop.md)\n";

    fn split(dir: &Path) {
        std::fs::create_dir_all(dir.join("docs/s")).unwrap();
        std::fs::write(dir.join("docs/s.md"), INDEX).unwrap();
        for (f, t) in [
            ("06-x.md", "SIX"),
            ("06.2-y.md", "SIXTWO"),
            ("06-x-2.md", "CONT"),
            ("07-loop.md", "SEVEN"),
        ] {
            std::fs::write(dir.join("docs/s").join(f), format!("{t}\n")).unwrap();
        }
    }

    fn idx() -> PathBuf {
        Path::new("docs").join("s.md")
    }

    #[test]
    fn an_unsplit_spec_is_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/s.md"), "# S\n\n## 1. A\n\ntext\n").unwrap();
        assert_eq!(spec_parts(&idx(), dir.path()), vec![idx()]);
        assert_eq!(whole_spec(&idx(), dir.path()), "# S\n\n## 1. A\n\ntext\n");
        assert_eq!(section_file(&idx(), "1", dir.path()), Some(idx()));
    }

    #[test]
    fn parts_list_in_index_order_with_continued_files() {
        let dir = tempfile::tempdir().unwrap();
        split(dir.path());
        let want: Vec<PathBuf> = ["06-x.md", "06.2-y.md", "06-x-2.md", "07-loop.md"]
            .iter()
            .map(|f| Path::new("docs").join("s").join(f))
            .collect();
        assert_eq!(spec_parts(&idx(), dir.path()), want);
    }

    #[test]
    fn whole_spec_is_the_opening_then_every_part() {
        let dir = tempfile::tempdir().unwrap();
        split(dir.path());
        let whole = whole_spec(&idx(), dir.path());
        assert!(whole.starts_with("# Spec\n\nOpening."));
        assert!(!whole.contains("## 6. Sessions"));
        assert!(whole.ends_with("SIX\nSIXTWO\nCONT\nSEVEN\n"));
    }

    #[test]
    fn section_file_finds_numbers_and_falls_back_to_the_h2_file() {
        let dir = tempfile::tempdir().unwrap();
        split(dir.path());
        let f = |n: &str| section_file(&idx(), n, dir.path());
        let part = |n: &str| Some(Path::new("docs").join("s").join(n));
        assert_eq!(f("7"), part("07-loop.md"));
        assert_eq!(f("6.2"), part("06.2-y.md"));
        assert_eq!(f("6.3"), part("06-x.md"));
        assert_eq!(f("6"), part("06-x.md"));
        assert_eq!(f("8"), None);
        assert_eq!(f("8.1"), None);
    }
}
