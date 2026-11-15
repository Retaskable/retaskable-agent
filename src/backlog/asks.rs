//! The store's chat ops (docs/specs/agent/06.6-ask-claude.md; Retaskable.com's team-state-on-server.md section
//! 3.3): `asks_list`, `ask_report`, `facts_get { kind: "audiences" }` and `draft_queue`. The chat's conversations are
//! the store's, not the agent's. Every failure is a `BacklogError`, never an empty answer.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::audiences::{Audience, audiences_of};
use super::{BacklogError, Client};

/// One message in the project's chat, as `asks_list` gives it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    pub id: String,
    pub text: String,
    /// The final answer, once there is one.
    #[serde(default)]
    pub answer: Option<String>,
    /// The ask this one replies to.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The backlog item or plan whose page it was asked on.
    #[serde(default)]
    pub about: Option<String>,
    /// `person` or `retaskable`.
    #[serde(default)]
    pub from: Option<String>,
    /// The draft a Retaskable message is about.
    #[serde(default)]
    pub draft_id: Option<String>,
    #[serde(default)]
    pub images: Vec<Value>,
    /// Cleared asks still count as the last five exchanges.
    #[serde(default)]
    pub cleared: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Any other field of the ask.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// What `ask_report` changes on an ask: only the fields given are sent.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// How long it has been thinking and what it is reading, live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Why it is waiting (paused, over budget, a drafter running).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// What each action the answer asked for came to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<Value>>,
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

#[derive(Deserialize)]
struct Asks {
    #[serde(default)]
    asks: Vec<Ask>,
}

#[derive(Deserialize)]
struct Queued {
    id: String,
}

impl Client {
    /// `asks_list {}`: the project's asks, cleared ones included.
    pub async fn asks_list(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Ask>, BacklogError> {
        let list: Asks = self.op(repo_id, token, "asks_list", json!({}), "asks").await?;
        Ok(list.asks)
    }

    /// `ask_report { id, ...fields }`: an ask's progress, waiting reason, final answer and action results.
    pub async fn ask_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        report: &AskReport,
    ) -> Result<(), BacklogError> {
        let mut args = serde_json::to_value(report).unwrap_or_else(|_| json!({}));
        args["id"] = json!(id);
        self.send(repo_id, token, "ask_report", args).await.map(|_| ())
    }

    /// `ask_report { from: "retaskable", text, draftId, replyTo? }`: a message of Retaskable's own, posted into
    /// the conversation of `reply_to` (a new one when `None`). Answers the new ask's id.
    pub async fn ask_post(
        &self,
        repo_id: u64,
        token: Option<&str>,
        text: &str,
        draft_id: &str,
        reply_to: Option<&str>,
    ) -> Result<String, BacklogError> {
        let mut args = json!({ "from": "retaskable", "text": text, "draftId": draft_id });
        if let Some(reply_to) = reply_to {
            args["replyTo"] = json!(reply_to);
        }
        let ask: Queued = self.op(repo_id, token, "ask_report", args, "ask").await?;
        Ok(ask.id)
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
        Ok(queued.id)
    }
}

#[cfg(test)]
mod tests;
