//! The store's project facts and draft queue: `facts_get { kind: "project" }` (the pause and today's spend) and
//! `draft_queue`. Every failure is a `BacklogError`, never an empty answer.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{BacklogError, Client};

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

/// A company audience (chat-asks-who.md), as `facts_get { kind: "audiences" }` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Audience {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub short: Option<String>,
    /// Who they are, in a sentence (the ideas prompt's user types).
    #[serde(default)]
    pub who: Option<String>,
    /// The part of the product they use.
    #[serde(default)]
    pub part: Option<String>,
}

#[derive(Deserialize)]
struct Audiences {
    #[serde(default)]
    audiences: Vec<Audience>,
}

/// What `facts_get { kind: "project" }` says about the project's use today.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFacts {
    /// The project's own pause (for every computer on it), set from the store.
    #[serde(default)]
    pub paused: bool,
    /// The store's own words for that pause ("Paused by @james for everyone").
    #[serde(default)]
    pub paused_reason: Option<String>,
    /// What the project's sessions cost today, in USD, across computers.
    #[serde(default)]
    pub spent_today_usd: f64,
}

#[derive(Deserialize)]
struct Queued {
    id: String,
}

impl Client {
    /// `facts_get { kind: "audiences" }`: the company's audiences.
    pub async fn audiences(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Audience>, BacklogError> {
        let list: Audiences = self
            .op(repo_id, token, "facts_get", json!({ "kind": "audiences" }), "audiences")
            .await?;
        Ok(list.audiences)
    }

    /// `facts_get { kind: "project" }`: the project's pause and today's spend, which gate new questions.
    pub async fn project_facts(&self, repo_id: u64, token: Option<&str>) -> Result<ProjectFacts, BacklogError> {
        self.op(
            repo_id,
            token,
            "facts_get",
            json!({ "kind": "project" }),
            "project facts",
        )
        .await
    }

    /// `draft_queue { request, titles, themes, audience, revises, askId }`: a new draft for the queue. Answers its id.
    pub async fn draft_queue(
        &self,
        repo_id: u64,
        token: Option<&str>,
        draft: &QueuedDraft,
    ) -> Result<String, BacklogError> {
        let queued: Queued = self.op(repo_id, token, "draft_queue", json!(draft), "draft").await?;
        Ok(queued.id)
    }
}

#[cfg(test)]
mod tests;
