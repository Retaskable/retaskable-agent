//! An expert session's work so far (docs/specs/expert-chat.md "The right panel's data"; Retaskable.com's
//! `agent/src/expert/work.ts`): what its branch changed and its commits, read from the lane's git after each turn so
//! it is the same on every engine, and the pull requests for its branch and its lane helpers' branches. Paths, counts
//! and subjects only.
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::git_bin::command;
use crate::implementer::publish::run as run_cmd;
use crate::merge_update::run as git;
use crate::text::clip;

/// The most files the summary lists, and commits.
const FILES: usize = 100;
const COMMITS: usize = 30;
/// The most pull requests a session reports.
const PRS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub subject: String,
    pub pushed: bool,
}

/// One pull request of the session (`ExpertPr`): no body, no diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `open`, `merged` or `closed`.
    pub state: &'static str,
    /// `none`, `running`, `passed` or `failed`.
    pub checks: &'static str,
    pub auto_merge: bool,
    pub branch: String,
    /// The helper whose branch (`<session's>-h<n>`) it is from; none for the session's own.
    pub helper: Option<u32>,
}

/// What the branch has changed since it left the base, its uncommitted changes included, and its commits, newest
/// first, and the pull requests (`ExpertWork`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Work {
    pub files: Vec<FileChange>,
    pub more: usize,
    pub commits: Vec<Commit>,
    pub prs: Vec<Pr>,
}

impl Work {
    pub fn to_json(&self) -> Value {
        let mut w = json!({
            "files": self.files.iter()
                .map(|f| json!({ "path": f.path, "added": f.added, "removed": f.removed }))
                .collect::<Vec<_>>(),
            "commits": self.commits.iter()
                .map(|c| json!({ "sha": c.sha, "subject": c.subject, "pushed": c.pushed }))
                .collect::<Vec<_>>(),
        });
        if self.more > 0 {
            w["more"] = json!(self.more);
        }
        if !self.prs.is_empty() {
            w["prs"] = json!(self.prs.iter().map(Pr::to_json).collect::<Vec<_>>());
        }
        w
    }

    /// Whether a pull request is still open, so it is worth reading again without a new turn.
    pub fn has_open_pr(&self) -> bool {
        self.prs.iter().any(|p| p.state == "open")
    }
}

impl Pr {
    fn to_json(&self) -> Value {
        let mut p = json!({
            "number": self.number, "title": self.title, "url": self.url, "state": self.state,
            "checks": self.checks, "autoMerge": self.auto_merge, "branch": self.branch,
        });
        if let Some(h) = self.helper {
            p["helper"] = json!(h);
        }
        p
    }
}

/// The files changed since the branch left `base_ref`, its uncommitted changes included, and its commits, newest
/// first. A commit is `pushed` unless it is among the newest `<remote>..HEAD` ones: all of them before the branch's
/// first push. The pull requests are not read here ([`prs`]).
pub fn read(dir: &Path, base_ref: &str, branch: &str) -> Result<Work> {
    let from = git(&command, dir, &["merge-base", base_ref, "HEAD"])?;
    let numstat = git(&command, dir, &["diff", "--numstat", "--no-renames", &from])?;
    let changed: Vec<FileChange> = numstat
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut parts = l.splitn(3, '\t');
            let (a, d, path) = (parts.next(), parts.next(), parts.next().unwrap_or(""));
            FileChange {
                path: clip(path, 300),
                added: a.and_then(|n| n.parse().ok()).unwrap_or(0),
                removed: d.and_then(|n| n.parse().ok()).unwrap_or(0),
            }
        })
        .collect();
    let range = format!("{from}..HEAD");
    let max = format!("-{COMMITS}");
    let log = git(&command, dir, &["log", &max, "--format=%h%x09%s", &range])?;
    let remote = format!("refs/remotes/origin/{branch}");
    let known = git(&command, dir, &["rev-parse", "--verify", "-q", &remote]).is_ok();
    let unpushed = if known {
        git(&command, dir, &["rev-list", "--count", &format!("{remote}..HEAD")])?
            .parse::<usize>()
            .unwrap_or(0)
    } else {
        usize::MAX
    };
    let commits = (log.lines().filter(|l| !l.is_empty()).enumerate())
        .map(|(i, l)| {
            let (sha, subject) = l.split_once('\t').unwrap_or((l, ""));
            Commit {
                sha: sha.to_string(),
                subject: clip(subject, 120),
                pushed: i >= unpushed,
            }
        })
        .collect();
    Ok(Work {
        more: changed.len().saturating_sub(FILES),
        files: changed.into_iter().take(FILES).collect(),
        commits,
        prs: Vec::new(),
    })
}

/// One row of `gh pr list --json number,title,url,state,headRefName,statusCheckRollup,autoMergeRequest` as a pull
/// request (`branchChangeOf`): the checks are failed when any has failed, passed when all are done, else running.
pub fn pr_of(p: &Value, helper: Option<u32>) -> Option<Pr> {
    let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let rollup: &[Value] = p
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let field = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).map(str::to_string);
    let done = |c: &Value| match field(c, "status") {
        Some(s) => s == "COMPLETED",
        None => !matches!(field(c, "state").as_deref(), Some("PENDING" | "EXPECTED")),
    };
    let bad = |c: &Value| {
        let word = field(c, "conclusion").or_else(|| field(c, "state")).unwrap_or_default();
        matches!(
            word.as_str(),
            "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "CANCELLED" | "ACTION_REQUIRED" | "ERROR"
        )
    };
    Some(Pr {
        number: p.get("number")?.as_u64()?,
        title: text("title"),
        url: text("url"),
        state: match p.get("state").and_then(Value::as_str) {
            Some("MERGED") => "merged",
            Some("CLOSED") => "closed",
            _ => "open",
        },
        checks: if rollup.is_empty() {
            "none"
        } else if rollup.iter().any(bad) {
            "failed"
        } else if rollup.iter().all(done) {
            "passed"
        } else {
            "running"
        },
        auto_merge: p.get("autoMergeRequest").is_some_and(|v| !v.is_null()),
        branch: text("headRefName"),
        helper,
    })
}

/// The pull requests from `head` in any state, as `gh pr list` (run in `dir`) lists them.
fn branch_prs(gh: &Path, dir: &Path, head: &str, helper: Option<u32>) -> Result<Vec<Pr>> {
    let fields = "number,title,url,state,headRefName,statusCheckRollup,autoMergeRequest";
    let args = [
        "pr", "list", "--head", head, "--state", "all", "--limit", "10", "--json", fields,
    ];
    crate::host_call::guard("gh", dir, &args)?;
    let mut list = std::process::Command::new(gh);
    list.current_dir(dir).args(args);
    let out = run_cmd(list, None, "gh pr list")?;
    let rows: Vec<Value> = serde_json::from_str(out.trim()).context("reading gh pr list's answer")?;
    Ok(rows.iter().filter_map(|r| pr_of(r, helper)).collect())
}

/// The pull requests for the session's branch and each lane helper's (`<branch>-h<n>`, `helpers` started), newest
/// first, the oldest dropped past ten. A branch the host can't read fails the whole read.
pub fn prs(gh: &Path, dir: &Path, branch: &str, helpers: u32) -> Result<Vec<Pr>> {
    let mut all = Vec::new();
    for n in 0..=helpers {
        let (head, helper) = if n == 0 {
            (branch.to_string(), None)
        } else {
            (format!("{branch}-h{n}"), Some(n))
        };
        let mut found = branch_prs(gh, dir, &head, helper)?;
        all.append(&mut found);
    }
    all.sort_by_key(|p| std::cmp::Reverse(p.number));
    all.truncate(PRS);
    Ok(all)
}

#[cfg(test)]
mod tests;
