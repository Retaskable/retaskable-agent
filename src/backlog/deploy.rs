//! The store's deploy request ops (docs/specs/agent/deploy-requests.md section 3.1, Retaskable.com's
//! deploy-requests-on-server.md): the website's TestFlight build, promote, hosting and go-live-prepare asks are rows
//! any computer may claim, but only one that holds what the request needs. This computer lists the open rows, claims
//! the ones it can run, beats while it runs and says how it ended. No secret is read or sent: a row holds the
//! command's own arguments, and its result only what the code answered. Every failure is an error.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// The longest a result may be, as the store counts it (`DEPLOY_RESULT_MAX_BYTES`).
pub const RESULT_MAX_BYTES: usize = 4 * 1024;

/// The top-level result keys the store refuses as a secret (`SECRET` in `shared/backlogDeployOps.ts`).
static SECRET_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)token|secret|password|credential|api[_-]?key|private").unwrap());

/// What a request asks for (`DEPLOY_KINDS`). `promote` with `now` is the old promote-now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployKind {
    TestflightBuild,
    Promote,
    Hosting,
    GoLivePrepare,
}

impl DeployKind {
    pub const ALL: [DeployKind; 4] = [Self::TestflightBuild, Self::Promote, Self::Hosting, Self::GoLivePrepare];

    pub fn name(self) -> &'static str {
        match self {
            Self::TestflightBuild => "testflight_build",
            Self::Promote => "promote",
            Self::Hosting => "hosting",
            Self::GoLivePrepare => "go_live_prepare",
        }
    }
}

/// A request's own arguments; only the fields its kind takes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployArgs {
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    /// A computer the person would rather ran it: ordered first, never required.
    #[serde(default)]
    pub prefer: Option<String>,
    #[serde(default)]
    pub now: Option<bool>,
    #[serde(default)]
    pub provider: Option<String>,
}

/// `DeployRequest.status`. A stale claim already shows `Requested` again in `deploy_list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployStatus {
    Requested,
    Claimed,
    Running,
    Done,
    Refused,
    Failed,
}

/// One row of the store's `deploy_requests` list, as `deploy_list` answers it. `claim` and `result` are not read here:
/// only whether the row is open.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployRequest {
    pub id: String,
    pub kind: DeployKind,
    #[serde(default)]
    pub args: DeployArgs,
    pub issued_by: String,
    /// When it was requested, in milliseconds.
    pub at: u64,
    pub status: DeployStatus,
}

/// How a request ended (`deploy_finish`'s `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Done,
    /// The code's own refusal (an HTTP 4xx in the TypeScript agent): the reason is in the result's `why`.
    Refused,
    Failed,
}

impl Finish {
    fn name(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

/// A result the store keeps (an object under 4 KB with no top-level key that names a secret), or why it can't be sent.
/// The store refuses the same ones; this answers before the round trip, so a bad result becomes a short `why` instead
/// of a lost report. Only the top level is checked, as the store does: a nested key is allowed.
pub fn checked_result(result: Map<String, Value>) -> Result<Map<String, Value>, String> {
    if let Some(key) = result.keys().find(|k| SECRET_KEY.is_match(k)) {
        return Err(format!("the result has a key that looks like a secret ({key})"));
    }
    let size = serde_json::to_vec(&result).map_or(usize::MAX, |b| b.len());
    if size > RESULT_MAX_BYTES {
        return Err(format!("the result is over {} KB", RESULT_MAX_BYTES / 1024));
    }
    Ok(result)
}

#[derive(Deserialize)]
struct Won {
    won: bool,
}

#[derive(Deserialize)]
struct Ok_ {
    ok: bool,
}

impl Client {
    /// `deploy_list`: the open and recent requests, oldest first, an expired claim shown `requested` again.
    pub async fn deploy_list(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<DeployRequest>, BacklogError> {
        self.op(repo_id, token, "deploy_list", json!({}), "deploy requests")
            .await
    }

    /// `deploy_claim { id, machine, can: true }`: whether this computer won the request. A computer that can't run it
    /// never calls this (it never declines on the site's behalf).
    pub async fn deploy_claim(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        machine: &str,
    ) -> Result<bool, BacklogError> {
        let args = json!({ "id": id, "machine": machine, "can": true });
        let won: Won = self.op(repo_id, token, "deploy_claim", args, "claim answer").await?;
        Ok(won.won)
    }

    /// `deploy_beat { id }`: whether this computer still holds the claim (`false`: it went stale and was reopened).
    pub async fn deploy_beat(&self, repo_id: u64, token: Option<&str>, id: &str) -> Result<bool, BacklogError> {
        let ok: Ok_ = self
            .op(repo_id, token, "deploy_beat", json!({ "id": id }), "beat answer")
            .await?;
        Ok(ok.ok)
    }

    /// `add { via: "site", items: [item] }`: the backlog item the website's Use this adds (go-live.md section 3). An id
    /// already on the list is the store's `conflict`.
    pub async fn add_site_item(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item: &impl Serialize,
    ) -> Result<(), BacklogError> {
        let args = json!({ "via": "site", "items": [item] });
        self.send(repo_id, token, "add", args).await.map(|_| ())
    }

    /// `deploy_finish { id, status, result? }`: whether the store took the report (`false`: the claim expired and
    /// another computer holds it, so this late result is refused).
    pub async fn deploy_finish(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        status: Finish,
        result: Option<Map<String, Value>>,
    ) -> Result<bool, BacklogError> {
        let mut args = json!({ "id": id, "status": status.name() });
        if let Some(result) = result {
            args["result"] = Value::Object(result);
        }
        let ok: Ok_ = self.op(repo_id, token, "deploy_finish", args, "finish answer").await?;
        Ok(ok.ok)
    }
}

#[cfg(test)]
mod tests;
