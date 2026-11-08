//! docs/specs/agent-ci.md section 3.3: a green pull request labelled `automerge` is squash-merged by the loop.
//! It takes the same `merge-<base>` store lock the orchestrator's merge takes (pieces.md section 3.4) and waits
//! for it, then merges only if main is still where the check saw it; else the new main is a new key, checked again.
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;

use super::{CheckKey, discovery};
use crate::backlog::Client;
use crate::backlog::runs::LockTake;
use crate::link::TokenStore;
use crate::runs::{CEILING_MARGIN, lock_ceiling};

/// The label that asks for the merge.
pub const AUTOMERGE_LABEL: &str = "automerge";

/// How often a lock another computer holds is asked for again.
pub const ASK_EVERY: Duration = Duration::from_secs(15);

/// The store's locks, so a test can stand in for it.
pub trait MergeLocks: Send + Sync {
    /// `lock_take` on the project's backlog.
    fn take<'a>(
        &'a self,
        github_repo: &'a str,
        name: &'a str,
        note: &'a str,
    ) -> BoxFuture<'a, Result<LockTake, String>>;
    /// `lock_drop`; a failure is the store's to expire.
    fn drop_lock<'a>(&'a self, github_repo: &'a str, token: &'a str) -> BoxFuture<'a, ()>;
}

/// What the loop merges with: the locks and how it waits for them.
#[derive(Clone)]
pub struct Automerge {
    pub locks: Arc<dyn MergeLocks>,
    pub ask_every: Duration,
    /// How long the lock is asked for: the `merge` lock's ceiling less its margin.
    pub wait: Duration,
}

impl Automerge {
    pub fn new(locks: Arc<dyn MergeLocks>) -> Self {
        Self {
            locks,
            ask_every: ASK_EVERY,
            wait: lock_ceiling("merge-").saturating_sub(CEILING_MARGIN),
        }
    }
}

/// The real locks: the backlog store, with the link's token from the keychain.
pub struct StoreLocks {
    url: String,
    keychain: Arc<dyn TokenStore>,
}

impl StoreLocks {
    pub fn new(url: String, keychain: Arc<dyn TokenStore>) -> Self {
        Self { url, keychain }
    }

    async fn ready(&self, github_repo: &str) -> Result<(Client, u64, String), String> {
        let token = self
            .keychain
            .get(&self.url)
            .map_err(|e| format!("reading the backlog link: {e:#}"))?
            .ok_or("this computer holds no link to the backlog store")?;
        let repo = crate::orchestrator::driver::repo_id(github_repo)
            .await
            .ok_or_else(|| format!("gh couldn't say the id of {github_repo}"))?;
        Ok((Client::new(self.url.clone()), repo, token))
    }
}

impl MergeLocks for StoreLocks {
    fn take<'a>(
        &'a self,
        github_repo: &'a str,
        name: &'a str,
        note: &'a str,
    ) -> BoxFuture<'a, Result<LockTake, String>> {
        Box::pin(async move {
            let (client, repo, token) = self.ready(github_repo).await?;
            client
                .lock_take(repo, Some(&token), name, Some(note))
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn drop_lock<'a>(&'a self, github_repo: &'a str, token: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Ok((client, repo, link)) = self.ready(github_repo).await {
                let _ = client.lock_drop(repo, Some(&link), token).await;
            }
        })
    }
}

/// Where the pull request is, and the program that merges it.
pub struct Target<'a> {
    pub gh: &'a OsStr,
    pub git: &'a OsStr,
    pub repo_dir: &'a Path,
    pub github_repo: &'a str,
    pub base: &'a str,
    pub computer: &'a str,
    pub key: &'a CheckKey,
}

/// How an automerge ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Merged,
    /// No `automerge` label, or the pull request is closed or has a new head.
    NotAsked,
    /// Main moved since the check: the new main is a new key.
    MainMoved,
    Failed(String),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    state: String,
    head_ref_oid: String,
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

async fn gh(target: &Target<'_>, args: &[&str]) -> Result<String, String> {
    let out = tokio::process::Command::new(target.gh)
        .args(args)
        .args(["--repo", target.github_repo])
        .current_dir(target.repo_dir)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("gh didn't start: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "gh {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Whether the pull request still asks for the merge: open, labelled `automerge`, and at the head that was checked.
async fn asked(t: &Target<'_>, pr: &str) -> Result<bool, String> {
    let view = gh(t, &["pr", "view", pr, "--json", "state,headRefOid,labels"]).await?;
    let view: View = serde_json::from_str(&view).map_err(|e| format!("unreadable pull request: {e}"))?;
    Ok(
        view.state == "OPEN"
            && view.head_ref_oid == t.key.head
            && view.labels.iter().any(|l| l.name == AUTOMERGE_LABEL),
    )
}

/// Merges the pull request of a green check, if it asks for it and main hasn't moved. Call only after success.
pub async fn run(am: &Automerge, t: &Target<'_>) -> Ended {
    let pr = t.key.pr.to_string();
    match asked(t, &pr).await {
        Ok(true) => {}
        Ok(false) => return Ended::NotAsked,
        Err(e) => return Ended::Failed(e),
    }
    let name = format!("merge-{}", t.base);
    let note = format!("PR #{} on {}", t.key.pr, t.computer);
    let token = match take(am, t, &name, &note).await {
        Ok(token) => token,
        Err(e) => return Ended::Failed(e),
    };
    let end = merge_if_still_main(t, &pr).await;
    am.locks.drop_lock(t.github_repo, &token).await;
    end
}

/// The lock's token, asking again while another computer holds it, up to `wait`.
async fn take(am: &Automerge, t: &Target<'_>, name: &str, note: &str) -> Result<String, String> {
    let start = tokio::time::Instant::now();
    loop {
        match am.locks.take(t.github_repo, name, note).await {
            Ok(LockTake::Held(token)) => return Ok(token),
            Ok(LockTake::HeldBy { by, .. }) if start.elapsed() >= am.wait => {
                return Err(format!("The {name} lock stayed held by {by}."));
            }
            Ok(LockTake::HeldBy { .. }) => {}
            Err(e) => return Err(format!("Couldn't take the {name} lock: {e}")),
        }
        tokio::time::sleep(am.ask_every).await;
    }
}

/// Under the lock (which may have been waited for a long time): the pull request is read again, main is read
/// again, and the squash merge goes only for the head and the main the check saw.
async fn merge_if_still_main(t: &Target<'_>, pr: &str) -> Ended {
    match asked(t, pr).await {
        Ok(true) => {}
        Ok(false) => return Ended::NotAsked,
        Err(e) => return Ended::Failed(e),
    }
    if let Err(e) = discovery::git(t.git, t.repo_dir, &["fetch", "origin", t.base]).await {
        return Ended::Failed(e);
    }
    match discovery::git(t.git, t.repo_dir, &["rev-parse", &format!("origin/{}", t.base)]).await {
        Ok(main) if main.trim() == t.key.main => {}
        Ok(_) => return Ended::MainMoved,
        Err(e) => return Ended::Failed(e),
    }
    match gh(
        t,
        &[
            "pr",
            "merge",
            pr,
            "--squash",
            "--delete-branch",
            "--match-head-commit",
            &t.key.head,
        ],
    )
    .await
    {
        Ok(_) => Ended::Merged,
        Err(e) => Ended::Failed(e),
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
