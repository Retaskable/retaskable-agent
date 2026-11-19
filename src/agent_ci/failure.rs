//! docs/specs/agent-ci.md section 3.5: what a failed run says about itself. Every failed run names the step that failed
//! (a status description such as `conflicts with main` or `test: 3 failed`), posts a comment with that step and the
//! log's last lines, and sends `ci_finish` the same as `step`, `excerpt` and, for a merge conflict, `reason: "conflict"`.
//! Before this the description was the log's last line that looked like a failure, which for a node test run that
//! passed its tests but exited non-zero in a later step was the summary `ℹ fail 0`.
use std::sync::OnceLock;

use regex::Regex;

use super::steps::Step;

/// `ci_finish`'s `reason` for a pull request that conflicts with main: the stable marker the store reads.
pub const REASON_CONFLICT: &str = "conflict";
/// The line the conflict pre-check writes to the log.
const CONFLICT_LINE: &str = "error: conflicts with main; merge main into the branch";
/// The most UTF-16 units of the excerpt sent to the store.
pub const EXCERPT_MAX: usize = 4000;
/// The lines of the log's end the excerpt is taken from.
const EXCERPT_LINES: usize = 40;

/// Why a run failed, in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The failing step's name: `conflicts with main`, `fetch`, `timed out`, `test`, or the last command run.
    pub step: String,
    /// The status description's tail: the step with its count or line (`test: 3 failed`).
    pub summary: String,
    /// `Some("conflict")` for a merge conflict.
    pub reason: Option<&'static str>,
    /// The log's last lines, never empty.
    pub excerpt: String,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

/// A test run's own summary line with no failures (`ℹ fail 0`, `# fail 0`, `0 failed`): not a reason.
fn zero_summary(line: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"(?i)\bfail(?:ed|ures?)?\s*[:=]?\s*0\b|\b0\s+(?:failed|failures?|errors?)\b",
    )
    .is_match(line)
}

/// The failed test count a run's summary gives (`ℹ fail 3`, `# fail 3`, `3 failed`).
fn failed_count(log: &str) -> Option<u64> {
    static A: OnceLock<Regex> = OnceLock::new();
    static B: OnceLock<Regex> = OnceLock::new();
    let a = re(&A, r"(?im)^.{0,3}?\s*(?:#\s*)?fail\s+(\d+)\s*$");
    let b = re(&B, r"(?i)\b(\d+)\s+failed\b");
    let last = |r: &Regex| r.captures_iter(log).filter_map(|c| c[1].parse::<u64>().ok()).last();
    last(a).or_else(|| last(b)).filter(|n| *n > 0)
}

/// The last command the check traced (`+ 17:51:07 npm run lint`), short.
fn last_command(log: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, r"^\++ \d\d:\d\d:\d\d (.+)$");
    log.lines()
        .rev()
        .filter_map(|l| r.captures(l.trim_end()).map(|c| c[1].to_string()))
        .find(|c| !c.starts_with("exit ") && !c.starts_with("echo "))
        .map(|c| c.chars().take(60).collect())
}

/// A line that looks like a failure but is not a zero summary.
fn failure_line(log: &str) -> Option<&str> {
    static R: OnceLock<Regex> = OnceLock::new();
    let r = re(&R, r"(?i)not ok|error|fail|✖|fatal|not found|denied|cannot|no such");
    log.trim_end().lines().rev().find(|l| r.is_match(l) && !zero_summary(l))
}

/// The last `EXCERPT_LINES` lines of the log, cut to `EXCERPT_MAX` UTF-16 units from the front; a log with nothing in
/// it says so.
pub fn excerpt_of(log: &str) -> String {
    let log = log.replace('\r', "");
    let lines: Vec<&str> = log.trim_end().lines().collect();
    let tail = lines[lines.len().saturating_sub(EXCERPT_LINES)..].join("\n");
    if tail.trim().is_empty() {
        return "(the run's log was empty)".into();
    }
    // Keep the end: drop characters from the front until it fits.
    let mut units = tail.encode_utf16().count();
    let mut out = tail.as_str();
    while units > EXCERPT_MAX {
        let c = out.chars().next().map_or(0, char::len_utf16);
        out = &out[out.chars().next().map_or(0, char::len_utf8)..];
        units -= c;
    }
    out.to_string()
}

impl Failure {
    /// What the log of a failed run (that failed in `step`) says.
    pub fn of(step: Step, log: &str) -> Self {
        let excerpt = excerpt_of(log);
        let (step_name, summary, reason): (String, String, Option<&'static str>) = if log.contains(CONFLICT_LINE) {
            (
                "conflicts with main".into(),
                "conflicts with main".into(),
                Some(REASON_CONFLICT),
            )
        } else if log.contains("error: timed out") {
            ("timed out".into(), "timed out".into(), None)
        } else if step == Step::Fetch {
            let why = failure_line(log).map_or(String::new(), |l| format!(": {}", l.trim()));
            ("fetch".into(), format!("fetch failed{why}"), None)
        } else if let Some(n) = failed_count(log) {
            ("test".into(), format!("test: {n} failed"), None)
        } else if let Some(l) = failure_line(log) {
            let cmd = last_command(log).unwrap_or_else(|| "check".into());
            (cmd.clone(), format!("{cmd}: {}", l.trim()), None)
        } else {
            let cmd = last_command(log).unwrap_or_else(|| "check".into());
            (cmd.clone(), format!("{cmd}: exited with an error"), None)
        };
        Self {
            step: step_name,
            summary,
            reason,
            excerpt,
        }
    }
}

#[cfg(test)]
mod tests;
