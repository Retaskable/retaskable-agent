//! The code host as the item flow's CI and merge steps read it: one `gh pr view` (`CodeHost.view` in the TS agent's
//! `codeHost/`), run in the project's checkout. Blocking `gh` calls are moved off the loop's thread here.
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::Ctx;
use crate::ci_pass::host::GhCiHost;

/// What `gh pr view` says of a change.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PrView {
    /// `OPEN`, `MERGED` or `CLOSED`.
    pub state: String,
    pub head_ref_oid: String,
    pub base_ref_name: String,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN`.
    pub mergeable: String,
    pub merge_state_status: String,
    pub created_at: Option<String>,
    pub auto_merge_request: Option<serde_json::Value>,
    pub is_in_merge_queue: bool,
    merge_commit: Option<Commit>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct Commit {
    oid: Option<String>,
}

impl PrView {
    /// The commit a merged change landed as.
    pub(super) fn merge_commit(&self) -> Option<String> {
        self.merge_commit.as_ref()?.oid.clone().filter(|o| !o.is_empty())
    }

    /// How old the change is, in milliseconds; `None` when the host doesn't say.
    pub fn age_ms(&self, now_ms: i64) -> Option<i64> {
        let made = chrono::DateTime::parse_from_rfc3339(self.created_at.as_deref()?).ok()?;
        Some(now_ms - made.timestamp_millis())
    }
}

const FIELDS: &str =
    "state,headRefOid,baseRefName,mergeable,mergeStateStatus,createdAt,autoMergeRequest,isInMergeQueue,mergeCommit";

fn view_in(gh: &Path, dir: &Path, pr: u64) -> anyhow::Result<PrView> {
    let number = pr.to_string();
    let (out, err, ok) = crate::ci_watch::gh_out(gh, dir, &["pr", "view", &number, "--json", FIELDS], &|_| {})?;
    anyhow::ensure!(ok, "gh pr view failed: {}", err.trim());
    Ok(serde_json::from_str(&out)?)
}

/// Epoch milliseconds now.
pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Ctx {
    /// The project's checkout, where `gh` runs.
    pub fn repo_dir(&self) -> PathBuf {
        PathBuf::from(&self.project.repo_path)
    }

    /// `gh pr view` of change `pr`; `Err` is the host's reason when it can't say.
    pub(super) async fn view(&self, pr: u64) -> Result<PrView, String> {
        let (gh, dir) = (self.gh.clone(), self.repo_dir());
        match tokio::task::spawn_blocking(move || view_in(&gh, &dir, pr)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// GitHub's CI host for the project, for its labels and reruns.
    pub fn ci_host(&self) -> GhCiHost {
        GhCiHost {
            gh: self.gh.clone(),
            dir: self.repo_dir(),
            repo: self.project.github_repo.clone(),
            base: self.project.base_branch.clone(),
            ready_label: crate::item_lifecycle::ci_when_ready_active(&self.project),
            worktrees: PathBuf::from(&self.project.worktrees_dir),
            git: std::sync::Arc::new(crate::git_bin::command),
        }
    }
}
