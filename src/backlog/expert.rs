//! The store's expert-queue ops (docs/specs/expert-queue-on-server.md section 3.1; Retaskable.com's
//! `shared/backlogExpertOps.ts`): an expert session's pending messages and the Claude Code session it continues.
//! This computer sends `expert_queue_add`, `expert_queue_take`, `expert_queue_done`, `expert_queue_clear`,
//! `expert_continues_set` and `expert_queue_list`; `expert_queue_edit` and `expert_queue_cancel` are the site's and
//! the app's writes, never this agent's. No path of this computer goes to the store: a continuation note holds a
//! session id and a title only. Every failure is an error, never an empty answer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// The store's code for a take another computer already won.
const CONFLICT: &str = "conflict";

/// Whether a queued message waits or a computer has taken it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowState {
    Queued,
    Taken,
}

/// The computer that took a row, and when it last said so (its beat).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowClaim {
    pub agent_id: String,
    pub machine: String,
    pub at: u64,
}

/// One pending message of an expert session, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertRow {
    pub id: String,
    pub text: String,
    /// The store's picture ids, never data.
    #[serde(default)]
    pub picture_ids: Vec<String>,
    /// When it was written, in milliseconds since the epoch.
    pub at: u64,
    /// The login of the sender.
    pub by: String,
    pub state: RowState,
    #[serde(default)]
    pub claim: Option<RowClaim>,
}

/// The Claude Code session an expert session continues: its id and title, never a path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpertFrom {
    pub session_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl ExpertFrom {
    /// Whether `session_id` is what the store takes: `[\w-]{1,100}`.
    pub fn session_id_ok(session_id: &str) -> bool {
        (1..=100).contains(&session_id.chars().count())
            && session_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }
}

/// What `expert_queue_list` answers: every session's rows, and the continuation notes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct QueueList {
    #[serde(default)]
    pub sessions: BTreeMap<String, Vec<ExpertRow>>,
    #[serde(default)]
    pub continues: BTreeMap<String, ExpertFrom>,
}

impl BacklogError {
    /// The store refused with `conflict`: another computer took the row, or the queue is full.
    pub fn is_conflict(&self) -> bool {
        self.code() == Some(CONFLICT)
    }
}

impl Client {
    /// `expert_queue_add { itemId, text, by, pictureIds? }`: a message sent while a turn runs waits for the
    /// session's next turn. `by` is the sender's login, which must be the person who started the session. Answers
    /// the row's id.
    pub async fn expert_queue_add(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item_id: &str,
        text: &str,
        by: &str,
        picture_ids: &[String],
    ) -> Result<String, BacklogError> {
        #[derive(Deserialize)]
        struct Added {
            id: String,
        }
        let mut args = json!({ "itemId": item_id, "text": text, "by": by });
        if !picture_ids.is_empty() {
            args["pictureIds"] = json!(picture_ids);
        }
        let added: Added = self
            .op(repo_id, token, "expert_queue_add", args, "queued message")
            .await?;
        Ok(added.id)
    }

    /// `expert_queue_take { itemId, ids?, machine }`: this computer takes rows of a session it runs (the oldest
    /// waiting row when `ids` is `None`). The first computer wins; another's take is a `conflict`. Taking again
    /// what this computer holds moves its claim on (the beat). Answers the rows now held, with the text the store
    /// has.
    pub async fn expert_queue_take(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item_id: &str,
        ids: Option<&[String]>,
        machine: &str,
    ) -> Result<Vec<ExpertRow>, BacklogError> {
        #[derive(Deserialize)]
        struct Taken {
            rows: Vec<ExpertRow>,
        }
        let mut args = json!({ "itemId": item_id, "machine": machine });
        if let Some(ids) = ids {
            args["ids"] = json!(ids);
        }
        let taken: Taken = self.op(repo_id, token, "expert_queue_take", args, "taken rows").await?;
        Ok(taken.rows)
    }

    /// `expert_queue_done { itemId, ids }`: the holder says the run has read these rows, so they leave the queue.
    /// `false` when it no longer holds all of them.
    pub async fn expert_queue_done(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item_id: &str,
        ids: &[String],
    ) -> Result<bool, BacklogError> {
        #[derive(Deserialize)]
        struct Done {
            ok: bool,
        }
        let args = json!({ "itemId": item_id, "ids": ids });
        let done: Done = self
            .op(repo_id, token, "expert_queue_done", args, "done answer")
            .await?;
        Ok(done.ok)
    }

    /// `expert_queue_clear { itemId }`: the session ended or was discarded, so its rows and its continuation note go.
    pub async fn expert_queue_clear(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item_id: &str,
    ) -> Result<(), BacklogError> {
        self.send(repo_id, token, "expert_queue_clear", json!({ "itemId": item_id }))
            .await
            .map(|_| ())
    }

    /// `expert_continues_set { itemId, from }`: the Claude Code session this one continues, set when the session
    /// starts.
    pub async fn expert_continues_set(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item_id: &str,
        from: &ExpertFrom,
    ) -> Result<(), BacklogError> {
        let args = json!({ "itemId": item_id, "from": from });
        self.send(repo_id, token, "expert_continues_set", args)
            .await
            .map(|_| ())
    }

    /// `expert_queue_list { itemId? }`: the waiting rows (a stale claim shows `queued` again) and the continuation
    /// notes, of one session or of every session.
    pub async fn expert_queue_list(
        &self,
        repo_id: u64,
        token: Option<&str>,
        item_id: Option<&str>,
    ) -> Result<QueueList, BacklogError> {
        let mut args = Value::Object(Map::new());
        if let Some(id) = item_id {
            args["itemId"] = json!(id);
        }
        self.op(repo_id, token, "expert_queue_list", args, "expert queue").await
    }
}

#[cfg(test)]
mod tests;
