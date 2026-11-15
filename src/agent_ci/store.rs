//! docs/specs/agent-ci.md section 3.4: the store ops the CI runner speaks, as `.agent-ci/runner.mjs` does:
//! `ci_claim` (the lease), `ci_heartbeat`, `ci_finish`, `ci_merged`, `ci_pr_closed` and `ci_merge_requests`. All go
//! over `POST <store>/v1/repos/<GitHub repo id>/ops` with this runner's own `ci` link token. A store that can't be
//! reached is an error: nothing new runs (fail closed).
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::link::{BACKLOG_TARGET, DeviceTokens, TokenAnswer, TokenStore};

/// The store, so a test can stand in for it. `op` answers the op's `value`.
pub trait CiStore: Send + Sync {
    fn op<'a>(&'a self, github_repo: &'a str, op: &'static str, args: Value) -> BoxFuture<'a, Result<Value, String>>;
}

/// `ci_claim`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claimed {
    Won,
    /// The check is already finished.
    Finished,
    /// Another computer holds the lease.
    Held(Option<String>),
}

/// What a claim sends: the commit, the status context and who is asking.
pub struct Claim<'a> {
    pub sha: &'a str,
    pub context: &'a str,
    pub event: &'a str,
    pub pr: u64,
    pub branch: &'a str,
    pub title: &'a str,
    pub author: &'a str,
    pub computer: &'a str,
}

/// The `ci_claim` args, as the runner builds them (a push has no pull request).
pub fn claim_args(c: &Claim) -> Value {
    let mut a = json!({ "sha": c.sha, "context": c.context, "event": c.event, "computer": c.computer });
    if c.pr != 0 {
        a["pr"] = json!(c.pr);
    }
    for (k, v) in [("branch", c.branch), ("title", c.title), ("author", c.author)] {
        if !v.is_empty() {
            a[k] = json!(v);
        }
    }
    a
}

/// Reads `ci_claim`'s answer: `{ won, finished?, holder? }`.
pub fn read_claim(v: &Value) -> Claimed {
    if v.get("won").and_then(Value::as_bool) == Some(true) {
        return Claimed::Won;
    }
    if v.get("finished").is_some_and(|f| !f.is_null() && f != &json!(false)) {
        return Claimed::Finished;
    }
    let holder = v.get("holder");
    let name = holder
        .and_then(|h| h.get("computer").or(Some(h)))
        .and_then(Value::as_str)
        .map(str::to_string);
    Claimed::Held(name)
}

/// A heartbeat that found the lease gone: `{ ok: false, holder? }`.
pub fn lease_lost(v: &Value) -> bool {
    v.get("ok").and_then(Value::as_bool) == Some(false)
}

/// The pull requests a person pressed Merge on: `ci_merge_requests`'s `{ requests: [{ pr, sha }] }`.
pub fn read_requests(v: &Value) -> Vec<(u64, String)> {
    v.get("requests")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|r| Some((r.get("pr")?.as_u64()?, r.get("sha")?.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// The real store: the backlog store at `url`, asked as the `ci` link.
pub struct StoreOps {
    tokens: DeviceTokens,
    http: reqwest::Client,
    ids: Mutex<std::collections::HashMap<String, u64>>,
}

impl StoreOps {
    pub fn new(url: String, keychain: Arc<dyn TokenStore>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            tokens: DeviceTokens::new(url, BACKLOG_TARGET, keychain, http.clone()),
            http,
            ids: Mutex::default(),
        }
    }

    /// The repo's GitHub id, asked of `gh` once.
    async fn repo_id(&self, github_repo: &str) -> Result<u64, String> {
        if let Some(id) = self.ids.lock().await.get(github_repo) {
            return Ok(*id);
        }
        let id = crate::orchestrator::driver::repo_id(github_repo)
            .await
            .ok_or_else(|| format!("gh couldn't say the id of {github_repo}"))?;
        self.ids.lock().await.insert(github_repo.to_string(), id);
        Ok(id)
    }

    async fn post(&self, id: u64, op: &str, args: &Value, stale: Option<&str>) -> Result<(u16, Value, String), String> {
        let (url, token) = match self.tokens.access_token(stale).await {
            TokenAnswer::Token { url, access_token } => (url, access_token),
            TokenAnswer::Idle => return Err("this computer's CI runner isn't linked to the store".into()),
            TokenAnswer::Unreachable => return Err("the store couldn't be reached".into()),
        };
        let res = self
            .http
            .post(format!("{url}/v1/repos/{id}/ops"))
            .bearer_auth(&token)
            .json(&json!({ "op": op, "args": args }))
            .send()
            .await
            .map_err(|e| format!("the store couldn't be reached: {e}"))?;
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or_default();
        Ok((status, body, token))
    }
}

impl CiStore for StoreOps {
    fn op<'a>(&'a self, github_repo: &'a str, op: &'static str, args: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let id = self.repo_id(github_repo).await?;
            let (mut status, mut body, token) = self.post(id, op, &args, None).await?;
            // A 401 asks again once with a fresh access token (storeLink.mjs).
            if status == 401 {
                (status, body, _) = self.post(id, op, &args, Some(&token)).await?;
            }
            if (200..300).contains(&status) {
                return Ok(body.get("value").cloned().unwrap_or(Value::Null));
            }
            let message = body
                .get("message")
                .or_else(|| body.get("error"))
                .and_then(Value::as_str)
                .map_or_else(|| format!("the store answered {status}"), str::to_string);
            Err(format!("{op}: {message}"))
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
