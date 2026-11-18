//! docs/specs/agent-ci.md section 3.3 (pools-on-server.md section 18.8): what the runner does with pull requests besides checking them.
//! The store decides which green pull requests merge: each poll the runner tells it the facts it read from the host (`prs`: each open pull
//! request's head, labels and approvals; `turn`: the open `retaskable/` pull requests that are up to date and whose check is pending) and it
//! answers `requests`. The runner merges only a request with `merge: true`, squash, pinned to the head that was checked, and holds no
//! label rule and no wait of its own: `merge: false` (with `holdUntil`) waits for a later poll. The store is told of each merge
//! (`ci_merged`) and of each pull request someone else closed (`ci_pr_closed`, once).
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::report::{self, Host};
use super::store::{self, CiStore, MergeRequest};
use super::timing::CiTiming;

/// What the loop merges with: the store and what it has read of it.
#[derive(Clone)]
pub struct Automerge {
    store: Arc<dyn CiStore>,
    /// What the store asked to merge, by `(pr, head sha)`, as of the last poll. Empty while the store can't be read: nothing merges.
    requests: Arc<Mutex<HashMap<(u64, String), MergeRequest>>>,
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
    /// The store has not asked for it, or the pull request is closed or has a new head.
    NotAsked,
    /// The store asked, with `merge: false`: it holds the merge (until `holdUntil`, epoch ms) and a later poll asks again.
    Held(Option<i64>),
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

impl Automerge {
    pub fn new(store: Arc<dyn CiStore>) -> Self {
        Self {
            store,
            requests: Arc::default(),
            told: Arc::default(),
            timing: Arc::default(),
        }
    }

    /// `github_repo`'s timing from the store's last answer, `None` while it is unknown.
    pub fn timing(&self, github_repo: &str) -> Option<CiTiming> {
        self.timing.lock().unwrap().get(github_repo).copied()
    }

    pub fn store(&self) -> &Arc<dyn CiStore> {
        &self.store
    }

    /// Tells the store the facts of this poll (`prs`, `turn`) and takes its answer: the merges it asks for and the repo's timing. A store
    /// that can't be read leaves no request and the timing unknown: nothing merges and nothing starts for the repo until it can be
    /// read (fail closed).
    pub async fn refresh_requests(&self, github_repo: &str, prs: Value, turn: &[u64]) {
        match self
            .store
            .op(github_repo, "ci_merge_requests", json!({ "prs": prs, "turn": turn }))
            .await
        {
            Ok(v) => {
                *self.requests.lock().unwrap() = store::read_requests(&v)
                    .into_iter()
                    .map(|r| ((r.pr, r.sha.clone()), r))
                    .collect();
                self.take_timing(github_repo, &v);
            }
            Err(e) => {
                tracing::warn!("agent CI: ci_merge_requests: {e}");
                self.requests.lock().unwrap().clear();
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

    /// What the store asked for this pull request at this head, if it did.
    pub fn request(&self, pr: u64, sha: &str) -> Option<MergeRequest> {
        self.requests.lock().unwrap().get(&(pr, sha.to_string())).cloned()
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

    /// The open `retaskable/` pull requests (`agents`: number and head) that are up to date with `base` and whose `/ci` check is pending:
    /// a fact for the store, which holds a person's merge while there is one. A pull request that can't be read is left out.
    pub async fn turn(&self, host: &Arc<Host>, base: &str, agents: &[(u64, String)]) -> Vec<u64> {
        let mut turn = Vec::new();
        for (n, sha) in agents {
            let Ok(compare) = get(host, format!("repos/{}/compare/{base}...{sha}", host.repo)).await else {
                continue;
            };
            if compare["behind_by"].as_u64() != Some(0) {
                continue;
            }
            let (h, sha) = (host.clone(), sha.clone());
            let Ok(Ok(statuses)) = tokio::task::spawn_blocking(move || h.read_statuses(&sha)).await else {
                continue;
            };
            if statuses
                .iter()
                .find(|s| s.context.ends_with("/ci"))
                .is_some_and(|s| s.state == "pending")
            {
                turn.push(*n);
            }
        }
        turn
    }

    /// Right after a check of `pr` at `head` ended green: tells the store the facts of the open pull requests (as a poll does), and
    /// merges this one only if the store answers with a merge request for it. Nothing here decides: no request, an unreadable list or
    /// store, or a status that is not green leaves the merge to the next poll.
    pub async fn after_green(&self, gh: &std::ffi::OsStr, host: &Arc<Host>, base: &str, pr: u64, head: &str) {
        let list = match super::prs::list(gh, &host.dir, &host.repo, base).await {
            Ok(l) => l,
            Err(e) => return tracing::warn!("agent CI: #{pr} after its check: {e}"),
        };
        let (Ok(facts), Ok(agents)) = (super::prs::facts(&list), super::prs::agent_branches(&list)) else {
            return;
        };
        let turn = self.turn(host, base, &agents).await;
        self.refresh_requests(&host.repo, facts, &turn).await;
        if self.request(pr, head).is_none() {
            return;
        }
        let h = host.clone();
        let sha = head.to_string();
        let Ok(Ok(all)) = tokio::task::spawn_blocking(move || h.read_statuses(&sha)).await else {
            return;
        };
        if super::claim::newest(&all, report::PR_CONTEXT).is_some_and(|s| s.state == "success") {
            self.run(host, pr, head).await;
        }
    }

    /// Merges pull request `pr` at `sha` (a green check's head) if the store asked for it with `merge: true` and it is still open at
    /// that head. Squash, pinned to `sha`.
    pub async fn run(&self, host: &Arc<Host>, pr: u64, sha: &str) -> Ended {
        let Some(request) = self.request(pr, sha) else {
            return Ended::NotAsked;
        };
        if !request.merge {
            return Ended::Held(request.held_until);
        }
        let p = match get(host, format!("repos/{}/pulls/{pr}", host.repo)).await {
            Ok(p) => p,
            Err(e) => return Ended::Failed(e),
        };
        if p["state"] != "open" || p["head"]["sha"] != sha {
            return Ended::NotAsked;
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
