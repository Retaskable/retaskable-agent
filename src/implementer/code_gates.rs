//! Code gates (docs/specs/agent/06.2-implementer.md, "Code gates before the push"): plain-code
//! checks on a lane's committed change against the base branch, run before Retaskable pushes it.
//! A source file the change touches mustn't be over `max_file_lines`; its added code mustn't
//! repeat `dup_lines` meaningful lines already somewhere in the project's tracked source; and a
//! spec it touches mustn't be over `max_spec_chars`. Ported from Retaskable.com's
//! `agent/src/codeGates.ts`; its `gateRule` is already ported as [`super::prompt::gate_rule`].
//! Nothing calls [`run`] yet: `src/orchestrator/session.rs`'s `ClaudeStarter` is the
//! orchestrator-level implementer step (`implement()` in `steps.ts`), but it doesn't push a change
//! yet, so there's nothing to gate before — including "if the gates can't run, that's noted and the
//! push goes ahead anyway", which is the caller's job, not this module's.
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use crate::config::CodeGates;
use crate::lanes;

/// Where a spec too big for [`CodeGates::max_spec_chars`] gets split into (docs/specs section
/// 6.10); this agent has no shared constant for it yet, unlike Retaskable.com's `specsDir.ts`.
const SPECS_DIR: &str = "docs/specs";
/// Files bigger than this aren't scanned for copies: they're data, not code.
const MAX_SCAN_BYTES: u64 = 1_000_000;
const MAX_SPLITS: usize = 3;
const MAX_COPIES: usize = 5;

static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(tsx?|jsx?|mjs|cjs|py|rs|go|java|kts?|swift|c|h|cc|cpp|hpp|cs|rb|php|scala|dart|lua|sh|ps1|vue|svelte)$").unwrap()
});
static SKIPPED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(^|/)(node_modules|vendor|dist|build)/|\.min\.|\.d\.ts$|\.generated\.").unwrap());
static TEST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|/)(tests?|__tests__)/|[._-](test|spec)\.[^./]+$|(^|/)test_[^/]*\.py$").unwrap()
});
/// Lines that repeat everywhere without being logic: comments, imports, re-exports.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^(//|/\*|\*|#|--|import\b|from\b|use\b|using\b|package\b|require\(|export\s.*\bfrom\s+["'])"#)
        .unwrap()
});
/// A top-level declaration, where a big file could be split (unindented, so not a method).
static ANCHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(export\s+)?(default\s+)?(async\s+)?(function|class|interface|type|const|enum)\s+[\w$]|^(pub(\([^)]*\))?\s+)?(fn|impl|struct|enum|trait|mod)\b|^(def|class)\s|^func\s|^((public|private|protected|internal)\s+)?(fun|object)\s",
    )
    .unwrap()
});
/// A spec: `specs/<name>.md`, or a split spec's section file `specs/<name>/<section>.md`.
static SPEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|/)specs/([^/]+/)?[^/]+\.md$").unwrap());
static HUNK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@@ -\S+ \+(\d+)(?:,(\d+))? @@").unwrap());
static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{2,3}) ").unwrap());
static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ {0,3}(```|~~~)").unwrap());

pub fn is_source(file: &str) -> bool {
    SOURCE.is_match(file) && !SKIPPED.is_match(file)
}

pub fn is_spec(file: &str) -> bool {
    SPEC.is_match(file) && !SKIPPED.is_match(file)
}

pub fn is_test(file: &str) -> bool {
    TEST.is_match(file)
}

/// A spec's size in characters, line endings counted as one, so it's the same on every computer.
pub fn spec_chars(text: &str) -> usize {
    text.replace("\r\n", "\n").chars().count()
}

fn split_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n").split('\n').map(str::to_string).collect()
}

pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let normalized = text.replace("\r\n", "\n");
    let total = normalized.split('\n').count();
    if normalized.ends_with('\n') { total - 1 } else { total }
}

/// What a session is sent back with, and the timeline's note, when a gate finds something.
#[derive(Debug, Clone, PartialEq)]
pub struct GateResult {
    pub message: String,
    pub note: String,
}

/// Each changed file's added line numbers (in the new file), from `git diff -U0`, in diff order. A
/// deleted file isn't included.
pub fn added_lines(diff: &str) -> Vec<(String, BTreeSet<u32>)> {
    let mut out: Vec<(String, BTreeSet<u32>)> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in diff.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(name) = line.strip_prefix("+++ ") {
            current = if name == "/dev/null" {
                None
            } else {
                // With core.quotepath off, only a path with a quote, tab or newline is still quoted.
                let unquoted = if name.len() >= 2 && name.starts_with('"') && name.ends_with('"') {
                    &name[1..name.len() - 1]
                } else {
                    name
                };
                let cleaned = unquoted.strip_prefix("b/").unwrap_or(unquoted);
                out.push((cleaned.to_string(), BTreeSet::new()));
                Some(out.len() - 1)
            };
            continue;
        }
        let Some(idx) = current else { continue };
        let Some(caps) = HUNK.captures(line) else { continue };
        let start: u32 = caps[1].parse().unwrap_or(0);
        let count: u32 = caps.get(2).map(|m| m.as_str().parse().unwrap_or(1)).unwrap_or(1);
        for n in start..start + count {
            out[idx].1.insert(n);
        }
    }
    out
}

fn lookup<'a>(added: &'a [(String, BTreeSet<u32>)], file: &str) -> Option<&'a BTreeSet<u32>> {
    added.iter().find(|(f, _)| f == file).map(|(_, s)| s)
}

/// Where a file could split: one top-level declaration near each equal-parts cut.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub line: u32,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BigFile {
    pub file: String,
    pub lines: usize,
    /// Only the first few files across one gate run get these (the shared `left` budget).
    pub splits: Vec<Split>,
}

fn truncate80(s: &str) -> String {
    s.trim().chars().take(80).collect()
}

/// Of `anchors` (each `(line, text, pos)`, `pos` along something `total` long), the ones nearest
/// the cuts into `parts` equal parts, at most `max`, in line order.
fn nearest(anchors: &[(u32, String, u32)], total: u32, parts: u32, max: usize) -> Vec<Split> {
    let mut splits: Vec<Split> = Vec::new();
    for k in 1..parts {
        if splits.len() >= max {
            break;
        }
        let target = (k as f64) * (total as f64) / (parts as f64);
        let mut best: Option<(u32, &str)> = None;
        let mut best_diff = f64::MAX;
        for (line, at, pos) in anchors {
            if *line <= 1 || splits.iter().any(|s| s.line == *line) {
                continue;
            }
            let diff = (*pos as f64 - target).abs();
            if diff < best_diff {
                best_diff = diff;
                best = Some((*line, at.as_str()));
            }
        }
        if let Some((line, at)) = best {
            splits.push(Split {
                line,
                at: at.to_string(),
            });
        }
    }
    splits.sort_by_key(|s| s.line);
    splits
}

/// The source files over `limit` lines, each with where it could split, at most [`MAX_SPLITS`]
/// split points shared across all of `files` (so a session isn't buried in splits to make).
pub fn oversized(files: &[(String, String)], limit: u32) -> Vec<BigFile> {
    let mut left = MAX_SPLITS;
    let mut out = Vec::new();
    for (file, text) in files {
        let lines = line_count(text);
        if lines as u32 <= limit {
            continue;
        }
        if left == 0 {
            out.push(BigFile {
                file: file.clone(),
                lines,
                splits: Vec::new(),
            });
            continue;
        }
        let anchors: Vec<(u32, String, u32)> = split_lines(text)
            .iter()
            .enumerate()
            .filter(|(_, l)| ANCHOR.is_match(l))
            .map(|(i, l)| ((i + 1) as u32, truncate80(l), (i + 1) as u32))
            .collect();
        let parts = ((lines as f64) / (limit as f64 * 0.8)).ceil() as u32;
        let splits = nearest(&anchors, lines as u32, parts, left);
        left -= splits.len();
        out.push(BigFile {
            file: file.clone(),
            lines,
            splits,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct BigSpec {
    pub file: String,
    pub chars: usize,
    /// The `##`/`###` headings nearest equal parts, always up to [`MAX_SPLITS`] per spec (no
    /// shared budget across specs, unlike [`oversized`]).
    pub splits: Vec<Split>,
}

/// The specs over `limit` characters, each with the headings to split it at (outside fenced code
/// blocks): `##` headings, or `###` when it has no `##` heading past its first line.
pub fn oversized_specs(files: &[(String, String)], limit: u32) -> Vec<BigSpec> {
    let mut out = Vec::new();
    for (file, text) in files {
        let chars = spec_chars(text);
        if chars as u32 <= limit {
            continue;
        }
        let mut heads: Vec<(u32, String, u32, u32)> = Vec::new();
        let mut pos: u32 = 0;
        let mut fenced = false;
        for (i, l) in split_lines(text).iter().enumerate() {
            if FENCE.is_match(l) {
                fenced = !fenced;
            }
            if !fenced && let Some(caps) = HEADING.captures(l) {
                heads.push(((i + 1) as u32, truncate80(l), pos, caps[1].len() as u32));
            }
            pos += l.chars().count() as u32 + 1;
        }
        let level = if heads.iter().any(|h| h.3 == 2 && h.0 > 1) {
            2
        } else {
            3
        };
        let filtered: Vec<(u32, String, u32)> = heads
            .into_iter()
            .filter(|h| h.3 == level)
            .map(|(line, at, pos, _)| (line, at, pos))
            .collect();
        let parts = ((chars as f64) / (limit as f64 * 0.8)).ceil() as u32;
        let splits = nearest(&filtered, chars as u32, parts, MAX_SPLITS);
        out.push(BigSpec {
            file: file.clone(),
            chars,
            splits,
        });
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct OtherRange {
    pub file: String,
    pub from: u32,
    pub to: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Copy {
    pub file: String,
    pub from: u32,
    pub to: u32,
    /// Where the same lines already are.
    pub other: OtherRange,
}

struct MeaningfulLine {
    line: u32,
    key: String,
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}

/// The lines that say something, normalised: blank lines, braces, comments and imports are left
/// out. Word characters are counted as ASCII alphanumerics/underscore (not Unicode `\w`), matching
/// the ported regex's lack of a `u` flag.
fn meaningful(text: &str) -> Vec<MeaningfulLine> {
    split_lines(text)
        .iter()
        .enumerate()
        .filter_map(|(i, raw)| {
            let key = collapse_whitespace(raw.trim());
            let word_chars = key.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').count();
            if word_chars >= 3 && !NOISE.is_match(&key) {
                Some(MeaningfulLine {
                    line: (i + 1) as u32,
                    key,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Runs of `size` meaningful lines the change added that are also somewhere else in `files` (the
/// project's tracked source, tests left out): in another file, elsewhere in the same file, or
/// added twice. Overlapping runs merge into one range. Code that was moved rather than copied
/// passes, since the old place no longer has it.
pub fn duplicates(files: &[(String, String)], added: &[(String, BTreeSet<u32>)], size: usize) -> Vec<Copy> {
    if size == 0 {
        return Vec::new();
    }
    let parsed: Vec<(String, Vec<MeaningfulLine>)> = files
        .iter()
        .map(|(file, text)| (file.clone(), meaningful(text)))
        .collect();

    struct Probe {
        file_idx: usize,
        idx: usize,
    }
    let mut all: Vec<Probe> = Vec::new();
    let mut matched: Vec<Option<(usize, usize)>> = Vec::new();
    let mut probes: HashMap<String, Vec<usize>> = HashMap::new();

    for (file_idx, (file, lines)) in parsed.iter().enumerate() {
        let Some(fresh) = lookup(added, file) else { continue };
        if is_test(file) || lines.len() < size {
            continue;
        }
        for i in 0..=(lines.len() - size) {
            if !lines[i..i + size].iter().all(|l| fresh.contains(&l.line)) {
                continue;
            }
            let key = lines[i..i + size]
                .iter()
                .map(|l| l.key.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let p_idx = all.len();
            all.push(Probe { file_idx, idx: i });
            matched.push(None);
            probes.entry(key).or_default().push(p_idx);
        }
    }
    if all.is_empty() {
        return Vec::new();
    }
    for (file_idx, (_, lines)) in parsed.iter().enumerate() {
        if lines.len() < size {
            continue;
        }
        for i in 0..=(lines.len() - size) {
            let key = lines[i..i + size]
                .iter()
                .map(|l| l.key.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let Some(found) = probes.get(&key) else { continue };
            for &p_idx in found {
                if matched[p_idx].is_some() {
                    continue;
                }
                let p = &all[p_idx];
                let far_enough = p.idx.abs_diff(i) >= size;
                if p.file_idx != file_idx || far_enough {
                    matched[p_idx] = Some((file_idx, i));
                }
            }
        }
    }

    let mut copies: Vec<Copy> = Vec::new();
    // (index into `all`/`matched`, index into `copies`) of the last emitted window, so an adjacent
    // overlapping one extends it instead of starting a new range.
    let mut last: Option<(usize, usize)> = None;
    for (p_idx, p) in all.iter().enumerate() {
        let Some((m_file_idx, m_idx)) = matched[p_idx] else {
            continue;
        };
        let p_lines = &parsed[p.file_idx].1;
        let m_lines = &parsed[m_file_idx].1;
        let end = p_lines[p.idx + size - 1].line;
        let other_end = m_lines[m_idx + size - 1].line;
        if let Some((last_p_idx, last_copy_idx)) = last {
            let last_p = &all[last_p_idx];
            let (last_m_file_idx, last_m_idx) = matched[last_p_idx].unwrap();
            if last_p.file_idx == p.file_idx
                && last_p.idx + 1 == p.idx
                && last_m_file_idx == m_file_idx
                && last_m_idx + 1 == m_idx
            {
                copies[last_copy_idx].to = end;
                copies[last_copy_idx].other.to = other_end;
                last = Some((p_idx, last_copy_idx));
                continue;
            }
        }
        copies.push(Copy {
            file: parsed[p.file_idx].0.clone(),
            from: p_lines[p.idx].line,
            to: end,
            other: OtherRange {
                file: parsed[m_file_idx].0.clone(),
                from: m_lines[m_idx].line,
                to: other_end,
            },
        });
        last = Some((p_idx, copies.len() - 1));
    }

    // Both copies new: say it once, not as each copying the other.
    let mut result: Vec<Copy> = Vec::new();
    for (i, c) in copies.iter().enumerate() {
        let mirrored = copies[..i].iter().any(|d| {
            d.file == c.other.file && d.from == c.other.from && d.other.file == c.file && d.other.from == c.from
        });
        if !mirrored {
            result.push(c.clone());
        }
    }
    result
}

struct Found {
    big: Vec<BigFile>,
    copies: Vec<Copy>,
    big_specs: Vec<BigSpec>,
    limit: u32,
    spec_limit: u32,
}

fn gate_message(found: &Found) -> String {
    let mut parts =
        vec!["Before Retaskable pushes your branch, its code gates found problems in your change:".to_string()];
    if !found.big.is_empty() {
        parts.push(format!("\n### Files over {} lines", found.limit));
        for b in &found.big {
            parts.push(format!("- `{}`: {} lines", b.file, b.lines));
            for s in &b.splits {
                parts.push(format!("  - could split at line {}: `{}`", s.line, s.at));
            }
        }
        parts.push(format!(
            "\nSplit each into smaller files by responsibility, each {} lines or fewer, without changing what the code does. Update the imports and tests that use it.",
            found.limit
        ));
    }
    if !found.copies.is_empty() {
        parts.push("\n### Duplicated code".to_string());
        for c in found.copies.iter().take(MAX_COPIES) {
            parts.push(format!(
                "- `{}:{}-{}` repeats `{}:{}-{}`",
                c.file, c.from, c.to, c.other.file, c.other.from, c.other.to
            ));
        }
        if found.copies.len() > MAX_COPIES {
            parts.push(format!("- and {} more", found.copies.len() - MAX_COPIES));
        }
        parts.push("\nReuse the existing code instead of copying it: call it, or move it into a shared function both places use.".to_string());
    }
    if !found.big_specs.is_empty() {
        parts.push(format!("\n### Specs over {} characters", found.spec_limit));
        for s in &found.big_specs {
            parts.push(format!("- `{}`: {} characters", s.file, s.chars));
            for at in &s.splits {
                parts.push(format!("  - could split at line {}: `{}`", at.line, at.at));
            }
        }
        parts.push(format!(
            "\nBreak it into a folder `{SPECS_DIR}/<name>/` with one file per top-level section (`NN-<slug>.md`), and keep `<name>.md` as a short index that lists each section's number, title and link, so references like '<name>.md section 7' still resolve. Move text only; don't rewrite it. A section file still over {} characters splits the same way at its `###` headings (`NN.M-<slug>.md`).",
            found.spec_limit
        ));
    }
    parts.push("\nFix them, commit, and return the result again.".to_string());
    parts.join("\n")
}

fn gate_note(found: &Found) -> String {
    let mut what = Vec::new();
    if !found.big.is_empty() {
        what.push(format!(
            "{} file{} over {} lines ({})",
            found.big.len(),
            if found.big.len() == 1 { "" } else { "s" },
            found.limit,
            found.big.iter().map(|b| b.file.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if !found.copies.is_empty() {
        what.push(format!(
            "{} copied block{}",
            found.copies.len(),
            if found.copies.len() == 1 { "" } else { "s" }
        ));
    }
    if !found.big_specs.is_empty() {
        what.push(format!(
            "{} spec{} over {} characters ({}; split one that's over into a section folder with an index)",
            found.big_specs.len(),
            if found.big_specs.len() == 1 { "" } else { "s" },
            found.spec_limit,
            found
                .big_specs
                .iter()
                .map(|s| s.file.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let joined = if what.len() > 1 {
        format!("{} and {}", what[..what.len() - 1].join(", "), what.last().unwrap())
    } else {
        what[0].clone()
    };
    format!("Code gates: {joined}")
}

/// The checks on the lane's committed change against `origin/<base_branch>...HEAD`, or `None` when
/// it passes (or `gates` has every limit at 0, turning the check off).
/// [`super::prompt::gate_rule`] is the prompts' matching sentence.
pub fn run(lane_dir: &Path, base_branch: &str, gates: &CodeGates) -> Result<Option<GateResult>> {
    if gates.max_file_lines == 0 && gates.dup_lines == 0 && gates.max_spec_chars == 0 {
        return Ok(None);
    }
    let range = format!("origin/{base_branch}...HEAD");
    let diff = lanes::git_output_in_worktree(
        lane_dir,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "-U0",
            "--no-renames",
            "--no-color",
            &range,
        ],
    )?;
    let added = added_lines(&diff);

    let mut big = Vec::new();
    if gates.max_file_lines > 0 {
        let mut files = Vec::new();
        for (f, _) in &added {
            let path = lane_dir.join(f);
            if is_source(f) && path.is_file() {
                files.push((f.clone(), std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
        big = oversized(&files, gates.max_file_lines);
    }

    let mut big_specs = Vec::new();
    if gates.max_spec_chars > 0 {
        let mut files = Vec::new();
        for (f, _) in &added {
            let path = lane_dir.join(f);
            if is_spec(f) && path.is_file() {
                files.push((f.clone(), std::fs::read_to_string(&path).unwrap_or_default()));
            }
        }
        big_specs = oversized_specs(&files, gates.max_spec_chars);
    }

    let mut copies = Vec::new();
    if gates.dup_lines > 0 {
        let tracked = lanes::git_output_in_worktree(lane_dir, &["ls-files", "-z"])?;
        let mut files = Vec::new();
        for f in tracked.split('\0') {
            if f.is_empty() || !is_source(f) || is_test(f) {
                continue;
            }
            let path = lane_dir.join(f);
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.len() > MAX_SCAN_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            files.push((f.to_string(), text));
        }
        copies = duplicates(&files, &added, gates.dup_lines as usize);
    }

    if big.is_empty() && copies.is_empty() && big_specs.is_empty() {
        return Ok(None);
    }
    let found = Found {
        big,
        copies,
        big_specs,
        limit: gates.max_file_lines,
        spec_limit: gates.max_spec_chars,
    };
    Ok(Some(GateResult {
        message: gate_message(&found),
        note: gate_note(&found),
    }))
}

#[cfg(test)]
#[path = "code_gates_tests.rs"]
mod tests;
