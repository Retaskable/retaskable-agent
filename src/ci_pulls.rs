//! The open pull requests the CI watcher looks at, and which of them it may push to (docs/specs/
//! ci-watch-through-store.md section 3, items 2 and 3). A port of `listPrs` (agent/src/gh.ts) and `mayPushTo`,
//! `mayPushOf` and `rememberLogin` (agent/src/ciWatch.ts). Reads and one pure rule: the pass that uses them, the
//! sessions and the pushes come later. The `gh` program is a parameter so tests can use a fake.
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ci_watch::{Reg, gh_out};

/// The most open pull requests looked at in one pass.
pub const LIST_MAX: usize = 100;

/// An open pull request as the watcher sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPull {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// Its own branch.
    pub branch: String,
    /// Its head commit.
    pub head: String,
    /// Who opened it, where `gh` says.
    pub author: Option<String>,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`, where `gh` says.
    pub mergeable: Option<String>,
    /// Its branch is in a fork (`isCrossRepository`).
    pub cross_repository: bool,
}

impl OpenPull {
    /// Whether it cannot be merged as it stands (`mergeable` is `CONFLICTING`).
    pub fn conflicting(&self) -> bool {
        self.mergeable.as_deref() == Some("CONFLICTING")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    mergeable: String,
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    is_cross_repository: bool,
}

#[derive(Deserialize)]
struct Author {
    #[serde(default)]
    login: String,
}

/// The open pull requests in `gh pr list --json`'s output. One with no head commit listed is left out, as `ciWatchPass`
/// does. A fork's pull request is listed like any other, as in TypeScript (whether the watcher may push to it is
/// `may_push_to`'s rule; a fork whose checks can't be read is the pass's to skip, one pull request at a time).
pub fn parse_open_pulls(json: &str) -> Result<Vec<OpenPull>> {
    let rows: Vec<Row> = serde_json::from_str(json.trim()).context("unreadable pull request list")?;
    Ok(rows
        .into_iter()
        .filter(|r| !r.head_ref_oid.is_empty())
        .map(|r| OpenPull {
            number: r.number,
            title: r.title,
            url: r.url,
            branch: r.head_ref_name,
            head: r.head_ref_oid,
            author: r.author.map(|a| a.login).filter(|l| !l.is_empty()),
            mergeable: Some(r.mergeable).filter(|m| !m.is_empty()),
            cross_repository: r.is_cross_repository,
        })
        .collect())
}

/// `gh pr list` for the open pull requests into `base`, at most `LIST_MAX`. `Err` is a `gh` error (no answer).
pub fn open_pulls(gh: &Path, dir: &Path, repo: &str, base: &str, reg: Reg) -> Result<Vec<OpenPull>> {
    let limit = LIST_MAX.to_string();
    let args = [
        "pr",
        "list",
        "--repo",
        repo,
        "--base",
        base,
        "--state",
        "open",
        "--limit",
        &limit,
        "--json",
        "number,title,url,headRefName,headRefOid,mergeable,author,isCrossRepository",
    ];
    let (out, err, ok) = gh_out(gh, dir, &args, reg)?;
    if !ok {
        bail!("gh pr list failed: {}", err.trim());
    }
    parse_open_pulls(&out)
}

/// Whether the agent may push to a pull request's branch: a `retaskable/` one, a Claude Code (`claude/`) one, one
/// opened by the account `gh` is signed in as, or one opened by someone on the project's `ciWatchAllow` (compared
/// without case). A pull request with no author listed is left to its branch alone.
pub fn may_push_to(branch: &str, author: Option<&str>, own_login: Option<&str>, allow: &[String]) -> bool {
    if branch.starts_with("retaskable/") || branch.starts_with("claude/") {
        return true;
    }
    let Some(author) = author.filter(|a| !a.is_empty()).map(str::to_lowercase) else {
        return false;
    };
    own_login.is_some_and(|own| own.to_lowercase() == author) || allow.iter().any(|a| a.to_lowercase() == author)
}

/// The login `gh` is signed in as, kept only once read: a failed or empty read gives `None` and is read again next
/// time, never remembered as "not signed in" (ci-watch-through-store.md section 2).
#[derive(Debug, Default)]
pub struct LoginCache(Mutex<Option<String>>);

impl LoginCache {
    /// The cached login, or what `read` gives now (kept when it is a non-empty answer).
    pub fn get(&self, read: impl FnOnce() -> Option<String>) -> Option<String> {
        if let Some(known) = self.0.lock().unwrap().clone() {
            return Some(known);
        }
        let login = read().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
        if let Some(l) = &login {
            *self.0.lock().unwrap() = Some(l.clone());
        }
        login
    }

    /// `may_push_to` for a pull request. The login is read only when the pull request lists an author; a failed
    /// read counts as no own account for this pass.
    pub fn may_push(&self, pull: &OpenPull, allow: &[String], read: impl FnOnce() -> Option<String>) -> bool {
        let own = pull.author.is_some().then(|| self.get(read)).flatten();
        may_push_to(&pull.branch, pull.author.as_deref(), own.as_deref(), allow)
    }
}

#[cfg(test)]
mod tests;
