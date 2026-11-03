//! The store's chat ops (docs/specs/agent/06.6-ask-claude.md; Retaskable.com's team-state-on-server.md section
//! 3.3): `asks_list`, `ask_report`, `facts_get { kind: "audiences" }` and `draft_queue`. The chat's conversations are
//! the store's, not the agent's. Every failure is a `BacklogError`, never an empty answer.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

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

/// A company audience (chat-asks-who.md), as `facts_get { kind: "audiences" }` lists it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Audience {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub short: Option<String>,
}

/// A draft for the store's queue (`draft_queue`). `audience` and `revises` are sent as `null` when there is none.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedDraft {
    pub request: String,
    pub titles: Vec<String>,
    pub themes: Vec<String>,
    pub audience: Option<String>,
    pub revises: Option<String>,
    /// The ask it came from.
    pub ask_id: String,
}

#[derive(Deserialize)]
struct Asks {
    #[serde(default)]
    asks: Vec<Ask>,
}

#[derive(Deserialize)]
struct Audiences {
    #[serde(default)]
    audiences: Vec<Audience>,
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

    /// `pause_get {}`: whether the project is paused on the store, for every computer on it.
    pub async fn project_paused(&self, repo_id: u64, token: Option<&str>) -> Result<bool, BacklogError> {
        #[derive(Deserialize)]
        struct Pause {
            paused: Option<Value>,
        }
        let pause: Pause = self.op(repo_id, token, "pause_get", json!({}), "pause").await?;
        Ok(pause.paused.is_some_and(|p| !p.is_null()))
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

    /// `facts_get { kind: "audiences" }`: the company's audiences.
    pub async fn audiences(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Audience>, BacklogError> {
        let list: Audiences = self
            .op(repo_id, token, "facts_get", json!({ "kind": "audiences" }), "audiences")
            .await?;
        Ok(list.audiences)
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
