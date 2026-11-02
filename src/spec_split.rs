//! Splits a long spec into an index plus section files (docs/specs/agent/06.10-spec-layout.md,
//! "A split spec" and "Splitting one"). Port of `scripts/splitSpec.ts`; `spec_parts` reads the
//! result back. Nothing is written unless every file fits the limit.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;

use crate::implementer::code_gates::{FENCE, spec_chars};
use crate::spec_parts::spec_parts;

/// The line the index carries once its spec is split (after the opening, before the sections).
pub const SPLIT_NOTE: &str = "This spec is split into the files below, one or more for each section.\n\n";

static BREAK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\r?\n[ \t]*\r?\n").unwrap());

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Already an index of section files: left alone.
    AlreadySplit,
    /// Already within the limit: left alone.
    Fits,
    /// The files written, relative to the root (the index first).
    Split(Vec<PathBuf>),
}

struct Entry {
    file: String,
    text: String,
    /// The heading's text for a `###` file, shown after its link.
    title: Option<String>,
    continued: bool,
}

/// Cuts `text` before each line starting with `marker` (`"## "`), outside code fences. The first
/// chunk is whatever comes before the first heading.
fn chunks(text: &str, marker: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        if FENCE.is_match(line) {
            fenced = !fenced;
        }
        if !fenced && line.starts_with(marker) && !out.last().unwrap().is_empty() {
            out.push(String::new());
        }
        out.last_mut().unwrap().push_str(line);
    }
    out
}

fn heading_text(chunk: &str) -> String {
    chunk
        .lines()
        .next()
        .unwrap_or("")
        .trim_start_matches('#')
        .trim()
        .to_string()
}

fn slug(title: &str) -> String {
    let words: Vec<String> = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
        .collect();
    let s: String = words.join("-").chars().take(40).collect();
    let s = s.trim_matches('-');
    if s.is_empty() { "section".into() } else { s.into() }
}

/// Each chunk's number: the one its heading starts with (`6.` → 6, `6.2` → 2) when all have one
/// and none repeat, else its place in the list.
fn numbers(titles: &[String], nth: usize) -> Vec<u32> {
    let parsed: Option<Vec<u32>> = titles
        .iter()
        .map(|t| {
            t.split(|c: char| !c.is_ascii_digit() && c != '.')
                .next()?
                .split('.')
                .filter(|p| !p.is_empty())
                .nth(nth)?
                .parse()
                .ok()
        })
        .collect();
    match parsed {
        Some(p) if p.iter().enumerate().all(|(i, n)| !p[..i].contains(n)) => p,
        _ => (1..=titles.len() as u32).collect(),
    }
}

/// Cuts at the paragraph break nearest `limit * 7/8` (35,000 of 40,000) until each piece fits;
/// a piece with no break to cut at is left over the limit for the caller to refuse.
fn cut(text: &str, limit: usize) -> Vec<String> {
    let target = limit / 8 * 7;
    let (mut pieces, mut rest) = (Vec::new(), text.to_string());
    while spec_chars(&rest) > limit {
        let best = BREAK
            .find_iter(&rest)
            .map(|m| m.end())
            .filter(|&e| e < rest.len())
            .min_by_key(|&e| spec_chars(&rest[..e]).abs_diff(target));
        let Some(at) = best else { break };
        let tail = rest.split_off(at);
        pieces.push(std::mem::replace(&mut rest, tail));
    }
    pieces.push(rest);
    pieces
}

/// A file's pieces as entries: `name`, then `name-2` ... listed as continued.
fn entries(name: &str, text: &str, title: Option<String>, limit: usize, out: &mut Vec<Entry>) {
    for (i, piece) in cut(text, limit).into_iter().enumerate() {
        let file = if i == 0 {
            format!("{name}.md")
        } else {
            format!("{name}-{}.md", i + 1)
        };
        out.push(Entry {
            file,
            text: piece,
            title: title.clone(),
            continued: i > 0,
        });
    }
}

fn plan(text: &str, limit: usize) -> (String, Vec<(String, Vec<Entry>)>) {
    let mut sections = chunks(text, "## ");
    let opening = sections.remove(0);
    let titles: Vec<String> = sections.iter().map(|s| heading_text(s)).collect();
    let nums = numbers(&titles, 0);
    let mut planned = Vec::new();
    for ((section, title), n) in sections.iter().zip(&titles).zip(nums) {
        let base = format!("{n:02}-{}", slug(title));
        let mut list = Vec::new();
        if spec_chars(section) <= limit {
            entries(&base, section, None, limit, &mut list);
        } else {
            let mut subs = chunks(section, "### ");
            let first = subs.remove(0);
            let subtitles: Vec<String> = subs.iter().map(|s| heading_text(s)).collect();
            entries(&base, &first, None, limit, &mut list);
            for ((sub, st), m) in subs.iter().zip(&subtitles).zip(numbers(&subtitles, 1)) {
                entries(
                    &format!("{n:02}.{m}-{}", slug(st)),
                    sub,
                    Some(st.clone()),
                    limit,
                    &mut list,
                );
            }
        }
        planned.push((title.clone(), list));
    }
    (opening, planned)
}

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"`[^`\n]*`|\]\(([^)\s]+)(\s+"[^"]*")?\)"#).unwrap());
static SCHEME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Za-z][A-Za-z0-9+.-]*:|/)").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ {0,3}#{1,6}[ \t]+(.+?)[ \t#]*\r?\n?$").unwrap());

/// GitHub's anchor for a heading: lowercase, punctuation dropped, spaces to hyphens.
fn gh_slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Lines outside code fences, each with whether it is a fence line's inside (false) or not.
fn prose_lines(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut fenced = false;
    text.split_inclusive('\n').map(move |line| {
        let fence = FENCE.is_match(line);
        if fence {
            fenced = !fenced;
        }
        (line, !fenced && !fence)
    })
}

/// Each heading's anchor and the file holding it (`""` for the index), in GitHub's order, so
/// repeats get `-1`, `-2`.
fn anchors(opening: &str, planned: &[(String, Vec<Entry>)]) -> HashMap<String, String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = HashMap::new();
    let texts = std::iter::once(("", opening)).chain(
        planned
            .iter()
            .flat_map(|(_, l)| l)
            .map(|e| (e.file.as_str(), e.text.as_str())),
    );
    for (file, text) in texts {
        for (line, prose) in prose_lines(text) {
            let Some(c) = prose.then(|| HEADING.captures(line)).flatten() else {
                continue;
            };
            let base = gh_slug(&c[1]);
            let n = seen.entry(base.clone()).or_insert(0);
            let a = if *n == 0 { base } else { format!("{base}-{n}") };
            *n += 1;
            out.entry(a).or_insert_with(|| file.to_string());
        }
    }
    out
}

/// Where a link written in the file `here` (`""` for the index) points once the spec is split.
fn retarget(t: &str, here: &str, stem: &str, anchors: &HashMap<String, String>) -> String {
    if SCHEME.is_match(t) {
        return t.to_string();
    }
    let (path, anchor) = t.split_once('#').map_or((t, None), |(p, a)| (p, Some(a)));
    if path.is_empty() || path == format!("{stem}.md") {
        if let Some((a, f)) = anchor.and_then(|a| Some((a, anchors.get(&a.to_lowercase())?))) {
            return match (f.as_str(), here) {
                (f, h) if f == h => format!("#{a}"),
                (f, "") => format!("{stem}/{f}#{a}"),
                ("", _) => format!("../{stem}.md#{a}"),
                (f, _) => format!("{f}#{a}"),
            };
        }
        if path.is_empty() {
            return t.to_string();
        }
    }
    if here.is_empty() {
        t.to_string()
    } else {
        format!("../{t}")
    }
}

/// Fixes the links in `text`, which moves from the index to the file `here` (`""`: stays in the
/// index). Code fences and inline code are left alone.
fn rewrite(text: &str, here: &str, stem: &str, anchors: &HashMap<String, String>) -> String {
    let mut out = String::new();
    for (line, prose) in prose_lines(text) {
        if !prose {
            out.push_str(line);
            continue;
        }
        out.push_str(&LINK.replace_all(line, |c: &regex::Captures| match c.get(1) {
            None => c[0].to_string(),
            Some(t) => format!(
                "]({}{})",
                retarget(t.as_str(), here, stem, anchors),
                c.get(2).map_or("", |m| m.as_str())
            ),
        }));
    }
    out
}

fn index_text(opening: &str, folder: &str, planned: &[(String, Vec<Entry>)], text: &str) -> String {
    let mut out = opening.to_string();
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
    }
    out.push_str(SPLIT_NOTE);
    let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("## ")).collect();
    for ((_, list), heading) in planned.iter().zip(headings) {
        out.push_str(&format!("{heading}\n\n"));
        for e in list {
            let path = format!("{folder}/{}", e.file);
            out.push_str(&format!("- [{path}]({path})"));
            if let Some(t) = e.title.as_ref().filter(|_| !e.continued) {
                out.push_str(&format!(": {t}"));
            }
            if e.continued {
                out.push_str(" (continued)");
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Splits `root/index_path` (e.g. `docs/specs/agent.md`) when it is over `limit` characters:
/// writes the index over it and the section files into the folder named for it. Refuses, writing
/// nothing, if any file would still be over the limit.
pub fn split_spec(root: &Path, index_path: &Path, limit: usize) -> Result<Outcome> {
    if spec_parts(index_path, root) != vec![index_path.to_path_buf()] {
        return Ok(Outcome::AlreadySplit);
    }
    let text = std::fs::read_to_string(root.join(index_path))?;
    if spec_chars(&text) <= limit {
        return Ok(Outcome::Fits);
    }
    let (opening, mut planned) = plan(&text, limit);
    if planned.is_empty() {
        bail!("{} has no `##` sections to split at", index_path.display());
    }
    let stem = index_path.file_stem().and_then(|s| s.to_str()).unwrap_or("spec");
    let found = anchors(&opening, &planned);
    let opening = rewrite(&opening, "", stem, &found);
    for e in planned.iter_mut().flat_map(|(_, l)| l) {
        e.text = rewrite(&e.text, &e.file, stem, &found);
    }
    let dir = index_path.parent().unwrap_or(Path::new(""));
    let folder = dir.join(stem);
    let index = index_text(&opening, stem, &planned, &text);
    let mut files = vec![(index_path.to_path_buf(), index)];
    for e in planned.into_iter().flat_map(|(_, list)| list) {
        files.push((folder.join(&e.file), e.text));
    }
    if let Some((path, _)) = files.iter().find(|(_, t)| spec_chars(t) > limit) {
        bail!(
            "{} would still be over {limit} characters, so nothing was split",
            path.display()
        );
    }
    for (path, text) in &files {
        if let Some(parent) = root.join(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(root.join(path), text)?;
    }
    Ok(Outcome::Split(files.into_iter().map(|(p, _)| p).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_parts::whole_spec;

    fn idx() -> PathBuf {
        Path::new("docs").join("s.md")
    }

    fn run(spec: &str, limit: usize) -> (tempfile::TempDir, Result<Outcome>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/s.md"), spec).unwrap();
        let r = split_spec(dir.path(), &idx(), limit);
        (dir, r)
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join("docs/s").join(name)).unwrap()
    }

    fn para(c: char, n: usize) -> String {
        format!("{}\n\n", c.to_string().repeat(n))
    }

    const OPENING: &str = "# Spec\n\nIntro.\n\n";

    #[test]
    fn sections_become_files_unchanged_and_read_back() {
        let spec = format!(
            "{OPENING}## 1. Alpha Beta\n\n{}## 2. Gamma\n\n{}",
            para('a', 250),
            para('g', 250)
        );
        let (dir, r) = run(&spec, 400);
        let Outcome::Split(files) = r.unwrap() else {
            panic!("not split")
        };
        assert_eq!(files.len(), 3);
        assert_eq!(
            read(dir.path(), "01-alpha-beta.md"),
            format!("## 1. Alpha Beta\n\n{}", para('a', 250))
        );
        assert_eq!(
            read(dir.path(), "02-gamma.md"),
            format!("## 2. Gamma\n\n{}", para('g', 250))
        );
        let index = std::fs::read_to_string(dir.path().join("docs/s.md")).unwrap();
        assert!(index.starts_with(OPENING) && index.contains("## 2. Gamma\n\n- [s/02-gamma.md](s/02-gamma.md)\n"));
        let whole = whole_spec(&idx(), dir.path());
        assert_eq!(whole.replace(SPLIT_NOTE, ""), spec);
        for f in files {
            assert!(spec_chars(&std::fs::read_to_string(dir.path().join(f)).unwrap()) <= 400);
        }
    }

    #[test]
    fn oversized_section_splits_at_h3_and_piece_at_the_paragraph_nearest_seven_eighths() {
        let big = format!("### 6.2 Deep\n\n{}{}{}", para('x', 150), para('y', 150), para('z', 150));
        let spec = format!("{OPENING}## 6. Sessions\n\nlead\n\n{big}### 6.3 Tail\n\ntail\n");
        let (dir, r) = run(&spec, 400);
        r.unwrap();
        assert_eq!(read(dir.path(), "06-sessions.md"), "## 6. Sessions\n\nlead\n\n");
        assert_eq!(read(dir.path(), "06.3-tail.md"), "### 6.3 Tail\n\ntail\n");
        // 350 is the target: the breaks fall near 168 and 320, so x and y stay together.
        let a = read(dir.path(), "06.2-deep.md");
        let b = read(dir.path(), "06.2-deep-2.md");
        assert_eq!(a, format!("### 6.2 Deep\n\n{}{}", para('x', 150), para('y', 150)));
        assert_eq!(b, para('z', 150));
        let index = std::fs::read_to_string(dir.path().join("docs/s.md")).unwrap();
        assert!(index.contains("(s/06.2-deep-2.md) (continued)"));
        assert_eq!(whole_spec(&idx(), dir.path()).replace(SPLIT_NOTE, ""), spec);
        assert_eq!(spec_parts(&idx(), dir.path()).len(), 4);
    }

    #[test]
    fn headings_in_code_fences_do_not_split() {
        let spec = format!(
            "{OPENING}## 1. A\n\n```\n## not a heading\n```\n\n{}## 2. B\n\n{}",
            para('a', 250),
            para('b', 250)
        );
        let (dir, r) = run(&spec, 400);
        r.unwrap();
        assert!(read(dir.path(), "01-a.md").contains("## not a heading"));
        assert_eq!(spec_parts(&idx(), dir.path()).len(), 2);
    }

    #[test]
    fn already_split_or_small_is_left_alone() {
        let spec = format!("{OPENING}## 1. A\n\n{}## 2. B\n\n{}", para('a', 250), para('b', 250));
        let (dir, r) = run(&spec, 400);
        r.unwrap();
        assert_eq!(split_spec(dir.path(), &idx(), 100).unwrap(), Outcome::AlreadySplit);
        let (_, r) = run(&spec, 5000);
        assert_eq!(r.unwrap(), Outcome::Fits);
    }

    #[test]
    fn refuses_and_writes_nothing_when_a_file_cannot_fit() {
        let spec = format!("{OPENING}## 1. A\n\nshort\n\n## 2. B\n\n{}", "b".repeat(500));
        let (dir, r) = run(&spec, 400);
        assert!(r.is_err());
        assert_eq!(std::fs::read_to_string(dir.path().join("docs/s.md")).unwrap(), spec);
        assert!(!dir.path().join("docs/s").exists());
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    fn split(spec: &str) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/s.md"), spec).unwrap();
        let Outcome::Split(files) = split_spec(dir.path(), Path::new("docs/s.md"), 500).unwrap() else {
            panic!("not split")
        };
        (dir, files)
    }

    fn read(dir: &Path, f: &str) -> String {
        std::fs::read_to_string(dir.join(f)).unwrap()
    }

    #[test]
    fn links_in_moved_text_keep_working() {
        let pad = "p".repeat(120);
        let spec = format!(
            "# S\n\nSee [o](#1-alpha) and [d](s.md#2-gamma) and [x](cloud.md).\n\n\
## 1. Alpha\n\n{pad}\n\n[x](cloud.md) [w](https://a.b/c.md) [m](mailto:a@b.c) [l](#1-alpha) \
[g](#2-gamma) [s](s.md#1-alpha) `[c](cloud.md)` [t](cloud.md#h \"T\")\n\n```\n[f](cloud.md)\n```\n\n\
## 2. Gamma\n\n{pad}\n\n[a](#1-alpha) [i](s.md) [e](#nope)\n"
        );
        let (dir, files) = split(&spec);
        let alpha = read(dir.path(), "docs/s/01-alpha.md");
        assert!(alpha.contains("[x](../cloud.md) [w](https://a.b/c.md) [m](mailto:a@b.c) [l](#1-alpha)"));
        assert!(alpha.contains("[g](02-gamma.md#2-gamma) [s](#1-alpha) `[c](cloud.md)` [t](../cloud.md#h \"T\")"));
        assert!(alpha.contains("```\n[f](cloud.md)\n```"));
        let gamma = read(dir.path(), "docs/s/02-gamma.md");
        assert!(gamma.contains("[a](01-alpha.md#1-alpha) [i](../s.md) [e](#nope)"));
        let index = read(dir.path(), "docs/s.md");
        assert!(index.contains("[o](s/01-alpha.md#1-alpha) and [d](s/02-gamma.md#2-gamma) and [x](cloud.md)."));
        // Every relative link between the split files resolves.
        for f in files {
            let text = read(dir.path(), &f.to_string_lossy());
            for (line, prose) in prose_lines(&text) {
                for c in LINK.captures_iter(line).filter(|_| prose) {
                    let Some(t) = c.get(1).map(|m| m.as_str()) else {
                        continue;
                    };
                    let path = t.split('#').next().unwrap();
                    if path.is_empty() || SCHEME.is_match(path) || path.contains("cloud.md") {
                        continue;
                    }
                    let at = dir.path().join(f.parent().unwrap()).join(path);
                    assert!(at.exists(), "{} links to {t}", f.display());
                }
            }
        }
    }

    #[test]
    fn anchors_follow_github_slugs() {
        assert_eq!(gh_slug("6.2 Implementer (v2)"), "62-implementer-v2");
        let planned = vec![];
        let a = anchors("# A\n\n## A\n", &planned);
        assert_eq!(a.get("a").map(String::as_str), Some(""));
        assert!(a.contains_key("a-1"));
    }
}
