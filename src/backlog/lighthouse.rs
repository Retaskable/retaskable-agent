//! The store ops the Lighthouse chore uses (docs/specs/daily-lighthouse.md sections 2 and 3; Retaskable.com's
//! agent/src/lighthouseChore.ts, teamDrafts.ts, backlogStore.ts): the run's raw result as a `lighthouse_raw` fact,
//! the team's chore drafts (each finding's draft status, and how many are open, as `chore_next` counts them) and the
//! raw per-page, per-preset reports kept in the store. Every failure is an error, never an empty answer.

use std::collections::BTreeMap;

use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// The store's finished `lighthouse` summary, answered to a `lighthouse_raw` `fact_set`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LighthouseSummary {
    pub day: String,
    /// The `LighthouseView` the store rolled up (scores, `days`, `a11y`, `security`), as it sent it.
    pub view: Value,
}

/// A store draft, as far as the chores care (`PublicDraft`, shared/backlogOps.ts).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoreDraft {
    pub id: String,
    /// `queued`, `drafting`, `needs_person`, `merged` or `discarded`.
    #[serde(default)]
    pub status: String,
    /// The holder's exact state beside the status; `failed` is not open.
    #[serde(default)]
    pub detail: Option<String>,
    /// A chore made it (not a person).
    #[serde(default)]
    pub chore: bool,
    /// Milliseconds since 1970.
    #[serde(default)]
    pub added_at: i64,
    /// Milliseconds since 1970: the draft's last timeline time.
    #[serde(default)]
    pub updated_at: i64,
}

impl ChoreDraft {
    /// Open the way `chore_next` counts: a chore's draft still queued, drafting or waiting on a person, and not failed.
    pub fn is_open_chore(&self) -> bool {
        self.chore
            && matches!(self.status.as_str(), "queued" | "drafting" | "needs_person")
            && self.detail.as_deref() != Some("failed")
    }
}

/// The team's drafts and how many chore drafts are open.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamDrafts {
    pub open: usize,
    pub drafts: Vec<ChoreDraft>,
}

impl TeamDrafts {
    /// A draft by its id.
    pub fn get(&self, id: &str) -> Option<&ChoreDraft> {
        self.drafts.iter().find(|d| d.id == id)
    }
}

/// The days of raw reports the store keeps, each day's file names.
pub type SavedRuns = BTreeMap<String, Vec<String>>;

impl Client {
    /// `fact_set { kind: "lighthouse_raw", data, machine }`: the run's raw result (per-page, per-preset scores,
    /// Core Web Vitals and findings, plus `newFindings`, `drafted?`, `note?` and each security check's
    /// `fixQueued`); answers the store's finished `lighthouse` summary.
    pub async fn lighthouse_set_raw(
        &self,
        repo_id: u64,
        token: Option<&str>,
        raw: &Value,
        machine: &str,
    ) -> Result<LighthouseSummary, BacklogError> {
        #[derive(Deserialize)]
        struct Row {
            data: LighthouseSummary,
        }
        let args = json!({ "kind": "lighthouse_raw", "data": raw, "machine": machine });
        let row: Row = self.op(repo_id, token, "fact_set", args, "lighthouse summary").await?;
        Ok(row.data)
    }

    /// `drafts {}`: the team's drafts, with how many chore drafts are open (as `chore_next` counts them).
    pub async fn team_drafts(&self, repo_id: u64, token: Option<&str>) -> Result<TeamDrafts, BacklogError> {
        let drafts: Vec<ChoreDraft> = self.op(repo_id, token, "drafts", json!({}), "drafts").await?;
        let open = drafts.iter().filter(|d| d.is_open_chore()).count();
        Ok(TeamDrafts { open, drafts })
    }

    /// `GET /lighthouse`: the days of raw reports the store keeps and each day's files (`savedRuns`).
    pub async fn saved_runs(&self, repo_id: u64, token: Option<&str>) -> Result<SavedRuns, BacklogError> {
        #[derive(Deserialize)]
        struct Days {
            days: SavedRuns,
        }
        let (status, bytes) = self.rest(self.http.get(self.report_url(repo_id, "")), token).await?;
        serde_json::from_value::<Days>(value_of(status, &bytes)?)
            .map(|d| d.days)
            .map_err(|e| bad_answer(status, format!("couldn't read the saved runs it sent: {e}")))
    }

    /// `GET /lighthouse/<day>/<file>`: one saved report's bytes (`latestSaved` reads these back).
    pub async fn saved_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        day: &str,
        file: &str,
    ) -> Result<Vec<u8>, BacklogError> {
        let url = self.report_url(repo_id, &format!("/{day}/{}", encode(file)));
        Ok(self.rest(self.http.get(url), token).await?.1)
    }

    /// `POST /lighthouse/<day>/<file>`: keeps one page and preset's raw report; answers the sha-256 the store holds
    /// for it, which the caller checks against its own before deleting the file.
    pub async fn upload_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        day: &str,
        file: &str,
        bytes: Vec<u8>,
    ) -> Result<String, BacklogError> {
        let url = self.report_url(repo_id, &format!("/{day}/{}", encode(file)));
        let req = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .body(bytes);
        let (status, body) = self.rest(req, token).await?;
        match value_of(status, &body)?.get("hash").and_then(Value::as_str) {
            Some(hash) => Ok(hash.to_string()),
            None => Err(bad_answer(status, "its answer had no hash".to_string())),
        }
    }

    fn report_url(&self, repo_id: u64, rest: &str) -> String {
        format!("{}/v1/repos/{}/lighthouse{rest}", self.base_url, repo_id)
    }

    /// Sends a request to a route that isn't an op: its status and body, a refusal as the store's own error.
    async fn rest(&self, mut req: RequestBuilder, token: Option<&str>) -> Result<(u16, Vec<u8>), BacklogError> {
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let res = req.send().await.map_err(|e| BacklogError::Unreachable(e.to_string()))?;
        let status = res.status();
        let bytes = res
            .bytes()
            .await
            .map_err(|e| BacklogError::Unreachable(e.to_string()))?
            .to_vec();
        if status.is_success() {
            return Ok((status.as_u16(), bytes));
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = body.get("error").and_then(Value::as_str).map(str::to_string);
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| code.clone())
            .unwrap_or_else(|| format!("the store answered {status}"));
        Err(BacklogError::BadAnswer {
            status: status.as_u16(),
            code,
            message,
        })
    }
}

/// The `value` of a store answer's JSON body.
fn value_of(status: u16, bytes: &[u8]) -> Result<Value, BacklogError> {
    let body: Value =
        serde_json::from_slice(bytes).map_err(|e| bad_answer(status, format!("couldn't read its answer: {e}")))?;
    match body.get("value") {
        Some(v) if !v.is_null() => Ok(v.clone()),
        _ => Err(bad_answer(status, "its answer had no value".to_string())),
    }
}

fn bad_answer(status: u16, message: String) -> BacklogError {
    BacklogError::BadAnswer {
        status,
        code: None,
        message,
    }
}

/// A file name as one path segment.
fn encode(file: &str) -> String {
    file.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests;
