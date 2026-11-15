//! docs/specs/agent-ci.md section 3.5: the part of a failed run's log worth posting, a port of
//! Retaskable.com's `.agent-ci/logExcerpt.mjs`: every failure line with its few lines after it, then the log's end.
//! A node test run's `not ok` lines come long before its end.
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

pub const MAX_LINES: usize = 200;
const TAIL: usize = 60;
const AFTER: usize = 4;
/// GitHub's comment limit is 65,536 characters.
const MAX_CHARS: usize = 60000;
/// The comment's lead line.
pub const LEAD: &str = "The failing lines and last lines of its log:";

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

fn fail(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"^\s*not ok\b|✖|error TS\d+|\bError:|npm ERR!|npm error|##\[error\]",
    )
    .is_match(line)
}

fn not_ok(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^\s*not ok\b").is_match(line)
}

fn ellipsis(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^\s*\.\.\.\s*$").is_match(line)
}

fn subtest(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^\s*# Subtest:").is_match(line)
}

/// The log's failing lines (each with what follows it) and its last lines, in at most `max` lines.
pub fn excerpt(text: &str, max: usize) -> String {
    let text = text.replace('\r', "");
    let lines: Vec<&str> = text.trim_end().split('\n').collect();
    if lines.len() <= max {
        return clip(&lines.join("\n"));
    }
    let tail_from = lines.len() - TAIL;
    // Kept lines and the gap markers between them share the budget, which also holds the last marker and the tail.
    let budget = max.saturating_sub(TAIL + 1);
    let keep = choose(&lines, tail_from, budget);
    let mut out: Vec<String> = Vec::new();
    let mut prev: isize = -1;
    for &i in &keep {
        let gap = i as isize > prev + 1;
        if out.len() + if gap { 2 } else { 1 } > budget {
            break;
        }
        if gap {
            out.push(format!("... ({} lines left out)", i as isize - prev - 1));
        }
        out.push(lines[i].to_string());
        prev = i as isize;
    }
    if tail_from as isize > prev + 1 {
        out.push(format!("... ({} lines left out)", tail_from as isize - prev - 1));
    }
    out.extend(lines[tail_from..].iter().map(|l| l.to_string()));
    clip(&out.join("\n"))
}

/// A failed test's own line: TAP's `not ok` or the spec reporter's `✖`.
fn failed_test(line: &str) -> bool {
    not_ok(line) || line.contains('✖')
}

/// The lines kept before `tail_from`, in at most `budget` lines counting gap markers. A failed test's own line comes
/// first, then the lines around it, then other error lines with theirs, so a log full of printed `Error:` lines
/// never pushes out the line saying which test failed.
fn choose(lines: &[&str], tail_from: usize, budget: usize) -> BTreeSet<usize> {
    // A failing line and what follows it, and for `not ok` its subtest line just before it.
    let block = |i: usize| -> Vec<usize> {
        let mut b = Vec::new();
        let mut end = i + AFTER;
        if not_ok(lines[i]) {
            // Its indented YAML diagnostics block, up to its `...`.
            if let Some(j) = (i + 1..tail_from.min(i + 41)).find(|&j| ellipsis(lines[j])) {
                end = end.max(j);
            }
            b.extend((i.saturating_sub(3)..i).rev().find(|&j| subtest(lines[j])));
        }
        b.extend(i..=end.min(tail_from - 1));
        b
    };
    let failed: Vec<usize> = (0..tail_from).filter(|&i| failed_test(lines[i])).collect();
    let other: Vec<usize> = (0..tail_from)
        .filter(|&i| fail(lines[i]) && !failed_test(lines[i]))
        .collect();
    let mut keep = BTreeSet::new();
    let lone = failed.iter().map(|&i| vec![i]);
    let around = failed.iter().map(|&i| block(i));
    let rest = other.iter().map(|&i| block(i));
    for wanted in lone.chain(around).chain(rest) {
        let mut next = keep.clone();
        next.extend(wanted);
        if rendered(&next) > budget {
            continue;
        }
        keep = next;
    }
    keep
}

/// How many lines `keep` takes once printed: each line, and a marker before each gap.
fn rendered(keep: &BTreeSet<usize>) -> usize {
    let mut prev: isize = -1;
    let mut n = 0;
    for &i in keep {
        n += if i as isize > prev + 1 { 2 } else { 1 };
        prev = i as isize;
    }
    n
}

/// Keeps the end of an overlong excerpt, line by line.
fn clip(s: &str) -> String {
    let n = s.chars().count();
    if n <= MAX_CHARS {
        return s.to_string();
    }
    let start = s.char_indices().nth(n - MAX_CHARS).map_or(0, |(i, _)| i);
    let cut = &s[start..];
    let from = cut.find('\n').map_or(0, |i| i + 1);
    format!("... (earlier lines left out)\n{}", &cut[from..])
}

/// The comment's lead line and log text: the failing lines and the log's end.
pub fn comment_log_text(text: &str) -> (&'static str, String) {
    (LEAD, excerpt(text, MAX_LINES))
}

#[cfg(test)]
mod tests;
