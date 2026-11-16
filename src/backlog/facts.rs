//! `facts_get`: the project's facts, one row per kind (spec 03.16). The store answers a map of
//! `{ kind: { kind, data, updatedAt, by, machine? } }`, leaving out a kind with no row. Beside it, what the
//! puller and the ideas path read and send: the project's pause (`pause_get`), the company's audiences and
//! `draft_queue`. Every failure is a `BacklogError`, never an empty answer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::audiences::{Audience, audiences_of};
use super::{BacklogError, Client};

/// One fact row.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    #[serde(default)]
    pub data: Value,
    /// When the row was last written (milliseconds); a writer sends it back as `ifUpdatedAt`.
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub machine: Option<String>,
}

/// A draft for the store's queue (`draft_queue`). `audience` and `revises` are sent as `null` when there is none; the
/// optional fields are left out when unset (chat sets only `ask_id`, an accepted idea the rest).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedDraft {
    pub request: String,
    pub titles: Vec<String>,
    pub themes: Vec<String>,
    pub audience: Option<String>,
    pub revises: Option<String>,
    /// The ask it came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ask_id: Option<String>,
    /// Who asked for it, and whom it is assigned to (`requestedBy`, `assignee`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Who pressed the button that started it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_by: Option<String>,
    /// What the drafter starts from beyond the request, e.g. `{ note }` (its first timeline line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intake: Option<Value>,
}

/// What `pause_get {}` says: the project's own pause, for every computer on it. Spend is this computer's own
/// (the machine store), not the store's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectFacts {
    pub paused: bool,
    /// Who paused it and why ("Paused by @james for everyone: before the demo").
    pub paused_reason: Option<String>,
}

/// The store's `ProjectPause` (`{ by, at, reason }`), `null` when not paused.
#[derive(Deserialize)]
struct PauseGet {
    #[serde(default)]
    paused: Option<PauseBy>,
}

#[derive(Deserialize)]
struct PauseBy {
    by: String,
    #[serde(default)]
    reason: Option<String>,
}

/// What `draft_queue` answers: `{ draft: { id }, joined? }` (a request on a card that already has a change waiting
/// joins it). A bare `{ id }` is read too.
#[derive(Deserialize)]
struct Queued {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    draft: Option<QueuedId>,
}

#[derive(Deserialize)]
struct QueuedId {
    id: String,
}

impl Client {
    /// `facts_get { kinds }`: the rows that exist, by kind. A kind with no row is left out of the answer.
    pub async fn facts_get(
        &self,
        repo_id: u64,
        token: Option<&str>,
        kinds: &[&str],
    ) -> Result<BTreeMap<String, Fact>, BacklogError> {
        self.op(repo_id, token, "facts_get", json!({ "kinds": kinds }), "facts")
            .await
    }

    /// `pause_get {}`: whether the project is paused for everyone, which gates new work and asks.
    pub async fn project_facts(&self, repo_id: u64, token: Option<&str>) -> Result<ProjectFacts, BacklogError> {
        let got: PauseGet = self.op(repo_id, token, "pause_get", json!({}), "pause").await?;
        Ok(match got.paused {
            None => ProjectFacts::default(),
            Some(p) => ProjectFacts {
                paused: true,
                paused_reason: Some(match p.reason.filter(|r| !r.trim().is_empty()) {
                    Some(reason) => format!("Paused by @{} for everyone: {reason}", p.by),
                    None => format!("Paused by @{} for everyone", p.by),
                }),
            },
        })
    }

    /// `facts_get { kinds: ["audiences"] }`: the company's audiences (the starters when it has no row yet).
    pub async fn audiences(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Audience>, BacklogError> {
        let facts = self.facts_get(repo_id, token, &["audiences"]).await?;
        Ok(audiences_of(
            &facts.get("audiences").map(|f| f.data.clone()).unwrap_or_default(),
        ))
    }

    /// `draft_queue { request, titles, themes, audience, revises, askId }`: a new draft for the queue. Answers its id.
    pub async fn draft_queue(
        &self,
        repo_id: u64,
        token: Option<&str>,
        draft: &QueuedDraft,
    ) -> Result<String, BacklogError> {
        let queued: Queued = self.op(repo_id, token, "draft_queue", json!(draft), "draft").await?;
        queued
            .draft
            .map(|d| d.id)
            .or(queued.id)
            .ok_or_else(|| BacklogError::BadAnswer {
                status: 200,
                code: None,
                message: "the draft it queued had no id".into(),
            })
    }
}

#[cfg(test)]
mod tests;
