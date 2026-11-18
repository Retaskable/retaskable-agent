//! docs/specs/agent-ci.md section 3.1 (What is checked): the open pull requests that are ready, each with the
//! key of its check, the pair (head sha, main sha). A new main sha makes a new key.
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

use crate::ci_label::READY_LABEL;
use crate::low_priority::low_priority_tokio;

/// One check to run: a pull request at a head merged with a main, or (`pr` 0) a push to main alone.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckKey {
    pub pr: u64,
    pub head: String,
    pub main: String,
}

impl CheckKey {
    /// The push to main's check: main's sha is the key (agent-ci.md section 3.2's last paragraph).
    pub fn push(main: &str) -> Self {
        Self {
            pr: 0,
            head: main.into(),
            main: main.into(),
        }
    }

    pub fn is_push(&self) -> bool {
        self.pr == 0
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    labels: Vec<Label>,
    head_ref_oid: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    latest_reviews: Vec<Review>,
}

#[derive(Deserialize)]
struct Review {
    #[serde(default)]
    state: String,
}

#[derive(Deserialize)]
struct Author {
    #[serde(default)]
    login: String,
}

/// What a claim tells the store of a pull request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrMeta {
    pub branch: String,
    pub title: String,
    pub author: String,
}

/// The open pull requests' details by number, from `gh pr list`'s output.
pub fn metas(json: &str) -> Result<std::collections::HashMap<u64, PrMeta>, String> {
    let prs: Vec<Pr> = serde_json::from_str(json).map_err(|e| format!("unreadable pull request list: {e}"))?;
    Ok(prs
        .into_iter()
        .map(|p| {
            let meta = PrMeta {
                branch: p.head_ref_name,
                title: p.title,
                author: p.author.map(|a| a.login).unwrap_or_default(),
            };
            (p.number, meta)
        })
        .collect())
}

/// The facts the store decides the merges from (`ci_merge_requests`' `prs`): each open pull request's number, head, label names and
/// the approving reviews it has now. Facts only: which label asks for a merge, and how many approvals are enough, are the store's.
pub fn facts(json: &str) -> Result<serde_json::Value, String> {
    let prs: Vec<Pr> = serde_json::from_str(json).map_err(|e| format!("unreadable pull request list: {e}"))?;
    Ok(prs
        .into_iter()
        .map(|p| {
            let approvals = p.latest_reviews.iter().filter(|r| r.state == "APPROVED").count();
            serde_json::json!({
                "pr": p.number,
                "sha": p.head_ref_oid,
                "labels": p.labels.into_iter().map(|l| l.name).collect::<Vec<_>>(),
                "approvals": approvals,
            })
        })
        .collect())
}

/// The open `retaskable/` pull requests' numbers and heads, for the turn the store holds a person's merge for.
pub fn agent_branches(json: &str) -> Result<Vec<(u64, String)>, String> {
    let prs: Vec<Pr> = serde_json::from_str(json).map_err(|e| format!("unreadable pull request list: {e}"))?;
    Ok(prs
        .into_iter()
        .filter(|p| p.head_ref_name.starts_with(AGENT_BRANCH_PREFIX))
        .map(|p| (p.number, p.head_ref_oid))
        .collect())
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

/// The branch prefix of an agent's pull request: it runs only once it carries `ci-ready`.
const AGENT_BRANCH_PREFIX: &str = "retaskable/";

/// The ready pull requests in `gh pr list --json number,isDraft,labels,headRefOid,headRefName,isCrossRepository`'s
/// output (agent-ci.md section 3.1): not drafts, not forks, and a `retaskable/` branch only with `ci-ready`.
pub fn ready(json: &str, main: &str) -> Result<Vec<CheckKey>, String> {
    let prs: Vec<Pr> = serde_json::from_str(json).map_err(|e| format!("unreadable pull request list: {e}"))?;
    Ok(prs
        .into_iter()
        .filter(|p| {
            !p.is_draft
                && !p.is_cross_repository
                && (!p.head_ref_name.starts_with(AGENT_BRANCH_PREFIX) || p.labels.iter().any(|l| l.name == READY_LABEL))
        })
        .map(|p| CheckKey {
            pr: p.number,
            head: p.head_ref_oid,
            main: main.to_string(),
        })
        .collect())
}

/// `gh pr list` for the open pull requests into `base`, run in the project's checkout so `gh` uses its login.
pub async fn list(gh: &OsStr, repo_dir: &Path, repo: &str, base: &str) -> Result<String, String> {
    let args = [
        "pr", "list", "--repo", repo, "--state", "open", "--base", base, "--limit", "200",
    ];
    crate::host_call::guard_program(gh, Some(repo_dir), &args).map_err(|e| e.to_string())?;
    let mut cmd = low_priority_tokio(gh);
    cmd.args(args)
        .args([
            "--json",
            "number,isDraft,labels,headRefOid,headRefName,isCrossRepository,title,author,latestReviews",
        ])
        .current_dir(repo_dir)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let out = cmd.output().await.map_err(|e| format!("gh didn't start: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
