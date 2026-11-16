//! A commit's CI as the open-pull-request watcher reads it (docs/specs/ci-watch-through-store.md section 3, item 2):
//! the commit's statuses and check runs through two `gh api` reads, the failed checks' logs and the note that names
//! them. A port of Retaskable.com's `commitCiStatus` and `commitChecksOf` (agent/src/gh.ts) and `ciFailedNote`
//! (agent/src/ciHealth.ts). The reads send raw facts to the store (`ci_result`); nothing here decides a verdict.
//! The `gh` program is a parameter so tests can use a fake.
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Deserialize;

use crate::backlog::ci_result::{CiChecks, CiState};
use crate::ci_logs;
use crate::ci_watch::{LOG_TAIL_LINES, Reg, gh_out};

/// The most characters of the failure note, as the store keeps them (`WHAT_MAX` there).
pub const WHAT_MAX: usize = 4000;
/// The most failed checks whose logs are read: the store takes at most this many.
const LOGS_MAX: usize = 10;

/// What a commit's checks add up to: `None` when no status or run was reported at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitState {
    None,
    Pending,
    Pass,
    Fail,
}

/// A check that failed or was cancelled (a cancelled run counts as failed), with when it ended, as GitHub wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    pub name: String,
    pub link: String,
    pub cancelled: bool,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitChecks {
    pub state: CommitState,
    pub failed: Vec<FailedCheck>,
}

#[derive(Deserialize)]
struct Status {
    #[serde(default)]
    context: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    target_url: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Deserialize)]
struct Run {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Pass,
    Pending,
    Fail,
    Cancel,
}

struct Row {
    name: String,
    bucket: Bucket,
    link: String,
    completed_at: Option<String>,
}

/// A commit's checks from its statuses and check runs. Pure; a run that was neutral or skipped is left out, a
/// cancelled one counts as failed, and a failed check carries when it ended.
fn commit_checks_of(statuses: &[Status], runs: &[Run]) -> CommitChecks {
    let from_status = |s: &Status| Row {
        name: s.context.clone(),
        bucket: match s.state.as_str() {
            "success" => Bucket::Pass,
            "pending" => Bucket::Pending,
            _ => Bucket::Fail,
        },
        link: s.target_url.clone().unwrap_or_default(),
        completed_at: if s.state == "pending" {
            None
        } else {
            s.updated_at.clone()
        },
    };
    let from_run = |r: &Run| Row {
        name: r.name.clone(),
        bucket: match (r.status.as_str(), r.conclusion.as_deref()) {
            ("completed", Some("success")) => Bucket::Pass,
            ("completed", Some("cancelled")) => Bucket::Cancel,
            ("completed", _) => Bucket::Fail,
            _ => Bucket::Pending,
        },
        link: r.html_url.clone().unwrap_or_default(),
        completed_at: r.completed_at.clone(),
    };
    let skipped = |r: &&Run| r.status == "completed" && matches!(r.conclusion.as_deref(), Some("neutral" | "skipped"));
    let rows: Vec<Row> = statuses
        .iter()
        .map(from_status)
        .chain(runs.iter().filter(|r| !skipped(r)).map(from_run))
        .collect();
    if rows.is_empty() {
        // Nothing reported at all (yet) is `None`; checks that all skipped are a pass, with nothing to wait for.
        let state = if statuses.is_empty() && runs.is_empty() {
            CommitState::None
        } else {
            CommitState::Pass
        };
        return CommitChecks {
            state,
            failed: Vec::new(),
        };
    }
    let failed: Vec<FailedCheck> = rows
        .iter()
        .filter(|r| matches!(r.bucket, Bucket::Fail | Bucket::Cancel))
        .map(|r| FailedCheck {
            name: r.name.clone(),
            link: r.link.clone(),
            cancelled: r.bucket == Bucket::Cancel,
            completed_at: r.completed_at.clone(),
        })
        .collect();
    let state = if !failed.is_empty() {
        CommitState::Fail
    } else if rows.iter().any(|r| r.bucket == Bucket::Pending) {
        CommitState::Pending
    } else {
        CommitState::Pass
    };
    CommitChecks { state, failed }
}

/// One `gh api repos/<repo>/commits/<sha>/<what>?per_page=100 --paginate --jq <jq>`, read as a list. All the pages are
/// read (the TS agent reads the first 100 only, so a failure on page 2 reads as a pass); `jq` runs on each page and
/// prints one object per line (`.statuses[]`), which is how the pages join into one list.
fn read_list<T: for<'de> Deserialize<'de>>(
    gh: &Path,
    dir: &Path,
    repo: &str,
    sha: &str,
    what: &str,
    jq: &str,
    reg: Reg,
) -> Result<Vec<T>> {
    let path = format!("repos/{repo}/commits/{sha}/{what}?per_page=100");
    let (out, err, ok) = gh_out(gh, dir, &["api", &path, "--paginate", "--jq", jq], reg)?;
    if !ok {
        let said = if err.trim().is_empty() { out.trim() } else { err.trim() };
        let said: String = said.chars().take(500).collect();
        bail!(
            "gh api commits/{}/{what} failed: {said}",
            sha.chars().take(7).collect::<String>()
        );
    }
    serde_json::Deserializer::from_str(&out)
        .into_iter::<T>()
        .collect::<Result<_, _>>()
        .with_context(|| {
            format!(
                "unreadable {what} of commit {}",
                sha.chars().take(7).collect::<String>()
            )
        })
}

/// The CI on a commit of `repo`: its latest status per context and its check runs. `Err` is `gh`'s reason when it
/// can't say (a token without the Checks permission fails here too: it is one pull request skipped, not a pass).
pub fn commit_checks(gh: &Path, dir: &Path, repo: &str, sha: &str, reg: Reg) -> Result<CommitChecks> {
    let statuses: Vec<Status> = read_list(gh, dir, repo, sha, "status", ".statuses[]", reg)?;
    let runs: Vec<Run> = read_list(gh, dir, repo, sha, "check-runs", ".check_runs[]", reg)?;
    Ok(commit_checks_of(&statuses, &runs))
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

/// The run id in a GitHub Actions check's link (`.../actions/runs/<id>/job/<job>`), or `None` for any other CI.
pub(crate) fn actions_run_id(link: &str) -> Option<&str> {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"github\.com/[^/]+/[^/]+/actions/runs/(\d+)")
        .captures(link)
        .map(|c| c.get(1).expect("one group").as_str())
}

/// The comment a check's link points at, where a CI posts its log as one (the CI runner's own: a pull request's
/// `#issuecomment-<id>`, a commit's `#commitcomment-<id>`): the `gh api` path of the comment.
fn log_comment_path(link: &str) -> Option<String> {
    static R: OnceLock<Regex> = OnceLock::new();
    let c = re(
        &R,
        r"github\.com/[^/]+/[^/]+/(?:pull|issues|commit)/[^#]+#(issue|commit)comment-(\d+)$",
    )
    .captures(link)?;
    let kind = c.get(1)?.as_str();
    let id = c.get(2)?.as_str();
    Some(if kind == "issue" {
        format!("issues/comments/{id}")
    } else {
        format!("comments/{id}")
    })
}

/// A failed check's log, cut to its failure lines and its end, as the host reads it (the comment a CI runner posted,
/// or a GitHub Actions run's failed steps); `None` where it can't be read. Like `ciLog` in `codeHost/github.ts`.
pub fn ci_log(gh: &Path, dir: &Path, repo: &str, check: &FailedCheck, reg: Reg) -> Option<String> {
    if let Some(path) = log_comment_path(&check.link) {
        let api = format!("repos/{repo}/{path}");
        let (out, _, ok) = gh_out(gh, dir, &["api", &api, "--jq", ".body"], reg).ok()?;
        let body = if ok { out.replace("\r\n", "\n") } else { String::new() };
        let fence = body.find("\n```\n")?;
        let end = body.rfind("\n```")?;
        if end <= fence {
            return None;
        }
        let text = ci_logs::excerpt(&body[fence + 5..end], LOG_TAIL_LINES * 2);
        return Some(text.trim().to_string()).filter(|t| !t.is_empty());
    }
    let id = actions_run_id(&check.link)?;
    let (out, _, ok) = gh_out(gh, dir, &["run", "view", id, "--repo", repo, "--log-failed"], reg).ok()?;
    if !ok {
        return None;
    }
    Some(ci_logs::excerpt(&out, LOG_TAIL_LINES).trim().to_string()).filter(|t| !t.is_empty())
}

/// Each failed check's log (`None` where it can't be read), for the first `LOGS_MAX` of them.
pub fn ci_logs(gh: &Path, dir: &Path, repo: &str, failed: &[FailedCheck], reg: Reg) -> Vec<Option<String>> {
    failed
        .iter()
        .take(LOGS_MAX)
        .map(|f| ci_log(gh, dir, repo, f, reg))
        .collect()
}

/// `log` unchanged within `max` characters; else its failure lines with their context and its end, the end alone
/// when it has no failure line. Differs from the TypeScript `failureExcerptChars` only in how the budget is
/// kept: it narrows the line excerpt until it fits, where that one counts characters as it picks lines.
pub fn keep_failures(log: &str, max: usize) -> String {
    if log.chars().count() <= max {
        return log.to_string();
    }
    let mut lines = log.lines().count().min(ci_logs::MAX_LINES);
    // The excerpt keeps a tail of 60 lines and a marker: nothing narrower than this is an excerpt.
    while lines > 64 {
        let out = ci_logs::excerpt(log, lines);
        if out.chars().count() <= max {
            return out;
        }
        lines = lines * 3 / 4;
    }
    keep_end(log, max)
}

/// `log`'s last lines within `max` characters, marking the lines left out above them.
pub fn keep_end(log: &str, max: usize) -> String {
    if log.chars().count() <= max {
        return log.to_string();
    }
    // A log's closing newline ends its last line; it doesn't start an empty one.
    let lines: Vec<&str> = log.trim_end_matches('\n').split('\n').collect();
    let mark = |left: usize| {
        format!(
            "... ({left} earlier line{} left out)\n",
            if left == 1 { "" } else { "s" }
        )
    };
    let mut kept = lines.len();
    let mut size = 0;
    while kept > 0 && size + lines[kept - 1].chars().count() + 1 + mark(kept - 1).chars().count() <= max {
        size += lines[kept - 1].chars().count() + 1;
        kept -= 1;
    }
    if kept == lines.len() {
        // Not even the last line fits: its own end.
        let m = mark(lines.len());
        let room = max.saturating_sub(m.chars().count());
        let last: Vec<char> = lines[kept - 1].chars().collect();
        let tail: String = last[last.len().saturating_sub(room)..].iter().collect();
        return format!("{m}{tail}").chars().take(max).collect();
    }
    format!("{}{}", mark(kept), lines[kept..].join("\n"))
}

fn note_of(failed: &[FailedCheck], logs: &[Option<String>], pr: &str) -> String {
    let list: Vec<String> = failed.iter().map(|f| format!("- {}: {}", f.name, f.link)).collect();
    let list = list.join("\n");
    let read: Vec<String> = failed
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let log = logs.get(i)?.as_deref().filter(|l| !l.is_empty())?;
            Some(format!(
                "The failing lines and end of {}'s log:\n\n```\n{log}\n```",
                f.name
            ))
        })
        .collect();
    if !read.is_empty() {
        return format!(
            "CI failed on your {pr}:\n\n{list}\n\n{}\n\nFix what failed, commit, and return the result again.",
            read.join("\n\n")
        );
    }
    // Sessions can't reach the network (no `gh`, no links), so with no log they reproduce the failure themselves.
    format!(
        "CI failed on your {pr}:\n\n{list}\n\nNo log was readable on this computer, so reproduce the failure first: run the project's checks for the failing step locally before changing anything. Then fix what failed, commit, and return the result again."
    )
}

/// What an implementer is told to read for each failed check: its link and its log's failing lines (`ciFailedNote`).
/// `budget`: the most characters the note may be; the logs give up their passing lines to fit it.
pub fn failed_note(failed: &[FailedCheck], logs: &[Option<String>], pr: &str, budget: Option<usize>) -> String {
    let note = note_of(failed, logs, pr);
    let len = note.chars().count();
    let Some(budget) = budget.filter(|&b| len > b) else {
        return note;
    };
    // Over the budget: each log gives up passing lines first, so the checks, the logs' failure lines and ends and
    // the closing instruction stay.
    let held: usize = logs.iter().flatten().map(|l| l.chars().count()).sum();
    let count = logs.iter().flatten().filter(|l| !l.is_empty()).count();
    let share = budget.saturating_sub(len - held) / count.max(1);
    let cut: Vec<Option<String>> = logs
        .iter()
        .map(|l| l.as_deref().filter(|l| !l.is_empty()).map(|l| keep_failures(l, share)))
        .collect();
    note_of(failed, &cut, pr)
}

/// When the latest of the failed checks ended, in milliseconds; `None` when none says.
fn ended_at(failed: &[FailedCheck]) -> Option<u64> {
    failed
        .iter()
        .filter_map(|f| chrono::DateTime::parse_from_rfc3339(f.completed_at.as_deref()?).ok())
        .filter_map(|t| u64::try_from(t.timestamp_millis()).ok())
        .max()
}

/// A head's checks as the facts `ci_result` takes: `pass` when checks reported and none is failing or pending,
/// `pending` (a commit with no checks reported yet included), or `fail` with the failure note, the failed checks'
/// latest ended. A pending head's `pendingSince` is the caller's to add: it is when this computer first saw it.
pub fn read_ci_checks(gh: &Path, dir: &Path, repo: &str, sha: &str, reg: Reg) -> Result<CiChecks> {
    let st = commit_checks(gh, dir, repo, sha, reg)?;
    Ok(match st.state {
        CommitState::Fail => {
            let logs = ci_logs(gh, dir, repo, &st.failed, reg);
            CiChecks {
                state: CiState::Fail,
                what: failed_note(&st.failed, &logs, "pull request", Some(WHAT_MAX)),
                logs,
                ended_at: ended_at(&st.failed),
                nothing_reported: false,
            }
        }
        // Nothing reported yet (a push GitHub hasn't registered checks for) is not a pass at once: it is pending, and
        // the pass reads it as "no CI" (a pass) only after the project's CI grace (`Pass::ci_grace_ms`). The TS agent
        // counts it a pass straight away and so can merge before checks exist.
        CommitState::Pending => CiChecks {
            state: CiState::Pending,
            what: String::new(),
            logs: Vec::new(),
            ended_at: None,
            nothing_reported: false,
        },
        CommitState::None => CiChecks {
            state: CiState::Pending,
            what: String::new(),
            logs: Vec::new(),
            ended_at: None,
            nothing_reported: true,
        },
        CommitState::Pass => CiChecks {
            state: CiState::Pass,
            what: String::new(),
            logs: Vec::new(),
            ended_at: None,
            nothing_reported: false,
        },
    })
}

#[cfg(test)]
mod tests;
