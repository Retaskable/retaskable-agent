//! docs/specs/agent-ci.md section 3.3: what the runner does with pull requests besides checking them, as
//! `.agent-ci/runner.mjs` does. A green pull request labelled `automerge`, or one a person pressed Merge on (the
//! store's `ci_merge_requests`), is squash-merged pinned to the head that was checked; there is no store lock. A
//! person's pull request waits (is not merged yet) while an up-to-date `retaskable/` pull request's check is
//! pending, for at most the store's `ciHoldMinutes` (0 merges at once). The store is told of each merge (`ci_merged`) and of each pull request someone
//! else closed (`ci_pr_closed`, once).
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use super::report::{self, Host};
use super::store::{self, CiStore};
use super::timing::CiTiming;

/// The label that asks for the merge.
pub const AUTOMERGE_LABEL: &str = "automerge";
/// The branch prefix of an agent's pull request.
pub const AGENT_BRANCH: &str = "retaskable/";

/// What the loop merges with: the store and what it has read of it.
#[derive(Clone)]
pub struct Automerge {
    store: Arc<dyn CiStore>,
    /// `(pr, head sha)` a person pressed Merge on, as of the last poll.
    asked: Arc<Mutex<HashSet<(u64, String)>>>,
    /// When each held merge was first held.
    held: Arc<Mutex<HashMap<u64, Instant>>>,
    /// Pull requests already told to the store as closed.
    told: Arc<Mutex<HashSet<u64>>>,
    /// Each repo's timing as its last `ci_merge_requests` said it (`ci`); a repo that has none (the store wasn't read, or sent
    /// none) is not in it, and nothing starts or merges for it.
    timing: Arc<Mutex<HashMap<String, CiTiming>>>,
}

/// How a merge attempt ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Merged,
    /// No `automerge` label or merge request, or the pull request is closed or has a new head.
    NotAsked,
    /// Waiting for this `retaskable/` pull request's turn; tried again on a later poll.
    Held(u64),
    Failed(String),
}

async fn get(host: &Arc<Host>, endpoint: String) -> Result<Value, String> {
    let h = host.clone();
    let out = tokio::task::spawn_blocking(move || h.api("GET", &endpoint, &[]))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))?;
    serde_json::from_str(&out).map_err(|e| format!("unreadable answer: {e}"))
}

fn is_agent_branch(p: &Value) -> bool {
    p["head"]["ref"].as_str().is_some_and(|r| r.starts_with(AGENT_BRANCH))
}

impl Automerge {
    pub fn new(store: Arc<dyn CiStore>) -> Self {
        Self {
            store,
            asked: Arc::default(),
            held: Arc::default(),
            told: Arc::default(),
            timing: Arc::default(),
        }
    }

    /// Says what the store's timing for `github_repo` is, as a poll would have read it (tests).
    #[cfg(test)]
    pub(crate) fn know(&self, github_repo: &str, timing: CiTiming) {
        self.timing.lock().unwrap().insert(github_repo.to_string(), timing);
    }

    /// `github_repo`'s timing from the store's last answer, `None` while it is unknown.
    pub fn timing(&self, github_repo: &str) -> Option<CiTiming> {
        self.timing.lock().unwrap().get(github_repo).copied()
    }

    pub fn store(&self) -> &Arc<dyn CiStore> {
        &self.store
    }

    /// Reads the pull requests a person pressed Merge on and the repo's timing (each poll). A store that can't be read leaves the
    /// last requests, and the timing unknown: nothing starts for the repo until it can be read (fail closed).
    pub async fn refresh_asked(&self, github_repo: &str) {
        match self.store.op(github_repo, "ci_merge_requests", json!({})).await {
            Ok(v) => {
                *self.asked.lock().unwrap() = store::read_requests(&v).into_iter().collect();
                self.take_timing(github_repo, &v);
            }
            Err(e) => {
                tracing::warn!("agent CI: ci_merge_requests: {e}");
                self.timing.lock().unwrap().remove(github_repo);
            }
        }
    }

    /// Keeps the `ci` of a store answer as `github_repo`'s timing; an answer with none leaves it unknown.
    fn take_timing(&self, github_repo: &str, answer: &Value) {
        let mut all = self.timing.lock().unwrap();
        match CiTiming::read(answer) {
            Some(t) => {
                all.insert(github_repo.to_string(), t);
            }
            None => {
                if all.remove(github_repo).is_some() {
                    tracing::warn!(
                        "agent CI: {github_repo}: the store sent no CI timing any more; nothing starts until it does"
                    );
                }
            }
        }
    }

    /// Whether a person pressed Merge on this pull request at this head.
    pub fn is_asked(&self, pr: u64, sha: &str) -> bool {
        self.asked.lock().unwrap().contains(&(pr, sha.to_string()))
    }

    /// Tells the store, once per pull request, of the 30 most recently updated closed pull requests whose head is
    /// in this repo.
    pub async fn tell_closed(&self, host: &Arc<Host>) {
        let endpoint = format!(
            "repos/{}/pulls?state=closed&sort=updated&direction=desc&per_page=30",
            host.repo
        );
        let closed = match get(host, endpoint).await {
            Ok(v) => v,
            Err(e) => return tracing::warn!("agent CI: closed pull requests: {e}"),
        };
        for p in closed.as_array().into_iter().flatten() {
            let Some(n) = p["number"].as_u64() else { continue };
            if self.told.lock().unwrap().contains(&n) || p["head"]["repo"]["full_name"].as_str() != Some(&host.repo) {
                continue;
            }
            let args = json!({ "pr": n, "merged": !p["merged_at"].is_null() });
            match self.store.op(&host.repo, "ci_pr_closed", args).await {
                Ok(_) => {
                    self.told.lock().unwrap().insert(n);
                }
                Err(e) => tracing::warn!("agent CI: ci_pr_closed #{n}: {e}"),
            }
        }
    }

    /// The open `retaskable/` pull request whose turn it is: up to date with `base` and its `/ci` check pending.
    async fn agent_turn(&self, host: &Arc<Host>, base: &str, open: &Value) -> Option<u64> {
        for p in open.as_array()? {
            if !is_agent_branch(p) {
                continue;
            }
            let (Some(sha), Some(n)) = (p["head"]["sha"].as_str(), p["number"].as_u64()) else {
                continue;
            };
            let compare = get(host, format!("repos/{}/compare/{base}...{sha}", host.repo))
                .await
                .ok()?;
            if compare["behind_by"].as_u64() != Some(0) {
                continue;
            }
            let (h, sha) = (host.clone(), sha.to_string());
            let statuses = tokio::task::spawn_blocking(move || h.read_statuses(&sha))
                .await
                .ok()?
                .ok()?;
            if statuses
                .iter()
                .find(|s| s.context.ends_with("/ci"))
                .is_some_and(|s| s.state == "pending")
            {
                return Some(n);
            }
        }
        None
    }

    /// Merges pull request `pr` at `sha` (a green check's head), if it still asks for it: open, at that head, and
    /// labelled `automerge` or asked for by a person. Squash, pinned to `sha`.
    pub async fn run(&self, host: &Arc<Host>, base: &str, pr: u64, sha: &str) -> Ended {
        let p = match get(host, format!("repos/{}/pulls/{pr}", host.repo)).await {
            Ok(p) => p,
            Err(e) => return Ended::Failed(e),
        };
        let labelled = p["labels"]
            .as_array()
            .is_some_and(|l| l.iter().any(|l| l["name"] == AUTOMERGE_LABEL));
        if p["state"] != "open" || p["head"]["sha"] != sha || !(labelled || self.is_asked(pr, sha)) {
            return Ended::NotAsked;
        }
        // How long a person's merge waits for the agent's turn is the store's `ciHoldMinutes`; unknown, nothing is merged.
        let Some(timing) = self.timing(&host.repo) else {
            return Ended::Failed("the store's CI timing isn't known yet".into());
        };
        if !is_agent_branch(&p) {
            let since = self.held.lock().unwrap().get(&pr).copied().unwrap_or_else(Instant::now);
            let holder = if since.elapsed() < timing.hold() {
                match get(host, format!("repos/{}/pulls?state=open&per_page=100", host.repo)).await {
                    Ok(open) => self.agent_turn(host, base, &open).await,
                    Err(_) => None,
                }
            } else {
                None
            };
            if let Some(turn) = holder {
                if self.held.lock().unwrap().insert(pr, since).is_none() {
                    tracing::info!("agent CI: merge #{pr}: waits for #{turn}, the agent's turn");
                }
                return Ended::Held(turn);
            }
            self.held.lock().unwrap().remove(&pr);
        }
        let (h, sha_s) = (host.clone(), sha.to_string());
        let endpoint = format!("repos/{}/pulls/{pr}/merge", host.repo);
        let merged = tokio::task::spawn_blocking(move || {
            h.api("PUT", &endpoint, &[("merge_method", "squash"), ("sha", &sha_s)])
        })
        .await;
        match merged {
            Ok(Ok(_)) => {
                // One try: a lost record only leaves the board's row Passed.
                let args = json!({ "sha": sha, "context": report::PR_CONTEXT, "pr": pr });
                if let Err(e) = self.store.op(&host.repo, "ci_merged", args).await {
                    tracing::warn!("agent CI: ci_merged: {e}");
                }
                Ended::Merged
            }
            Ok(Err(e)) => Ended::Failed(format!("{e:#}")),
            Err(e) => Ended::Failed(e.to_string()),
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests;
