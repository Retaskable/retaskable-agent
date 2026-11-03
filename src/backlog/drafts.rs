//! The store's draft ops (docs/specs/agent/06.5-spec-drafts.md; Retaskable.com's team-state-on-server.md section
//! 3.3 and spec-queue.md): `draft_claim_next`, `draft_report`, `draft_answer_taken`, `draft_front`,
//! `upsert_from_specs` and the `add` of a chat fix. A claim's token is the draft's lease, sent back by `draft_report`
//! and `draft_answer_taken`. Every failure is an error, never an empty answer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BacklogError, ClaimNextAgent, Client};
use crate::spec_items::SpecItem;

/// A person's answers to a draft's questions, one per question in order.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftAnswer {
    #[serde(default)]
    pub answers: Vec<String>,
    #[serde(default)]
    pub by: Option<String>,
}

/// What a queued draft carries to the computer that writes it: the chat's parse of the request.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftIntake {
    /// The theme of each parsed title, in the titles' order.
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub trial: bool,
    #[serde(default)]
    pub parsed: bool,
    #[serde(default)]
    pub do_next: bool,
    #[serde(default)]
    pub note: Option<String>,
}

/// The draft `draft_claim_next` hands out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedDraft {
    pub id: String,
    /// The request's text.
    #[serde(default)]
    pub request: Option<String>,
    /// The titles the chat parsed the request into.
    #[serde(default)]
    pub titles: Vec<String>,
    #[serde(default)]
    pub intake: Option<DraftIntake>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub answer: Option<DraftAnswer>,
    #[serde(default)]
    pub rebase_reason: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    /// Any other field of the draft.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// A claimed draft and the token that leases it to this computer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DraftClaim {
    pub draft: ClaimedDraft,
    pub token: String,
}

/// A draft taken for a person's press (`draft_take`): the store's draft and the lease that goes with it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TakenDraft {
    pub draft: ClaimedDraft,
    pub token: String,
}

/// What `draft_report` changes: only the fields given are sent.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftReport {
    /// The store's statuses: `queued`, `drafting`, `needs_person`, `merged`, `discarded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The holder's exact state beside the status: `awaiting_approval`, `needs_clarification`, `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The drafter's session, so a restart resumes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub titles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Every backlog entry the draft added, in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_ids: Option<Vec<String>>,
    /// The drafter's checked breakdown, as the store's details take it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<Value>,
}

/// A fix the drafter returned: added as a `todo` item "Fix: …" (chat-specs.md section 13.5).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixItem {
    pub id: String,
    pub title: String,
    pub kind: &'static str,
    pub spec_refs: Vec<String>,
    pub remaining: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
}

impl FixItem {
    /// A fix with `title` as given (`Fix: ` is put in front when it lacks it), cut to the store's 300 characters.
    pub fn new(id: impl Into<String>, title: &str, remaining: &str) -> Self {
        let title = title.trim();
        let title = if title.to_lowercase().starts_with("fix:") {
            title.to_string()
        } else {
            format!("Fix: {title}")
        };
        Self {
            id: id.into(),
            title: title.chars().take(300).collect(),
            kind: "fix",
            spec_refs: Vec::new(),
            remaining: remaining.trim().chars().take(4000).collect(),
            requested_by: None,
        }
    }
}

/// Which headers `upsert_from_specs` sends, and what it knows about them.
#[derive(Debug, Clone, Copy)]
pub struct SpecsUpsert<'a> {
    pub commit: &'a str,
    /// The commit the store took headers from last (its `specs_commit`).
    pub previous: Option<&'a str>,
    pub items: &'a [SpecItem],
    /// Ids an approved draft added: they start `todo`.
    pub approved: &'a [String],
    /// Who asked for the plan each approved id came from.
    pub asked_by: &'a BTreeMap<String, String>,
}

impl Client {
    /// `draft_claim_next { agent, engines? }`: takes the first draft waiting for a computer, or `None`.
    /// `engines` is what this computer's drafter would start on, so the store skips it while limited.
    pub async fn draft_claim_next(
        &self,
        repo_id: u64,
        token: Option<&str>,
        agent: &ClaimNextAgent,
        engines: &[String],
    ) -> Result<Option<DraftClaim>, BacklogError> {
        let mut args = json!({ "agent": agent });
        if !engines.is_empty() {
            args["engines"] = json!(engines);
        }
        let (status, body) = self.send(repo_id, token, "draft_claim_next", args).await?;
        match body.value {
            None | Some(Value::Null) => Ok(None),
            Some(value) => serde_json::from_value(value)
                .map(Some)
                .map_err(|e| BacklogError::BadAnswer {
                    status,
                    code: None,
                    message: format!("couldn't read the draft it claimed: {e}"),
                }),
        }
    }

    /// `draft_report { id, token, ...fields }`: the holder's change to its draft.
    pub async fn draft_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        report: &DraftReport,
    ) -> Result<(), BacklogError> {
        let mut args = serde_json::to_value(report).unwrap_or_else(|_| json!({}));
        args["id"] = json!(id);
        args["token"] = json!(lease);
        self.send(repo_id, token, "draft_report", args).await.map(|_| ())
    }

    /// `draft_take { id, agent }`: takes a draft nobody holds (or this computer's own) to land or close it.
    pub async fn draft_take(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        agent: &ClaimNextAgent,
    ) -> Result<TakenDraft, BacklogError> {
        let args = json!({ "id": id, "agent": agent });
        self.op(repo_id, token, "draft_take", args, "taken draft").await
    }

    /// `draft_release { id, token }`: lets go of a draft taken but not acted on.
    pub async fn draft_release(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
    ) -> Result<(), BacklogError> {
        let args = json!({ "id": id, "token": lease });
        self.send(repo_id, token, "draft_release", args).await.map(|_| ())
    }

    /// `draft_answer_taken { id, token }`: the holder took the person's answer, so the store clears it.
    pub async fn draft_answer_taken(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
    ) -> Result<(), BacklogError> {
        let args = json!({ "id": id, "token": lease });
        self.send(repo_id, token, "draft_answer_taken", args).await.map(|_| ())
    }

    /// `draft_front { id }`: a do-next press moves a draft waiting for a computer to the front (`conflict`
    /// when it isn't waiting).
    pub async fn draft_front(&self, repo_id: u64, token: Option<&str>, id: &str) -> Result<(), BacklogError> {
        self.send(repo_id, token, "draft_front", json!({ "id": id }))
            .await
            .map(|_| ())
    }

    /// `upsert_from_specs`: the parsed headers at a commit; answers the ids it added.
    pub async fn upsert_from_specs(
        &self,
        repo_id: u64,
        token: Option<&str>,
        upsert: &SpecsUpsert<'_>,
    ) -> Result<Vec<String>, BacklogError> {
        #[derive(Deserialize)]
        struct Added {
            added: Vec<String>,
        }
        let args = json!({
            "commit": upsert.commit,
            "previous": upsert.previous,
            "items": upsert.items,
            "approved": upsert.approved,
            "askedBy": upsert.asked_by,
        });
        let answer: Added = self
            .op(repo_id, token, "upsert_from_specs", args, "upsert answer")
            .await?;
        Ok(answer.added)
    }

    /// `add { via: "chat", items: [fix] }`: a `todo` item "Fix: …". An id already taken is a `conflict`.
    pub async fn add_fix(&self, repo_id: u64, token: Option<&str>, fix: &FixItem) -> Result<(), BacklogError> {
        self.send(repo_id, token, "add", json!({ "via": "chat", "items": [fix] }))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests;
