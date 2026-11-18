//! The store's intent rows (Retaskable.com's `shared/backlogIntentOps.ts`, docs/specs/intent-rows.md sections 3.1 and
//! 3.2): `intent_list`, `intent_claim`, `intent_beat` and `intent_finish`, and a chat picture's bytes. The website
//! writes what a person wants as a row, a computer that can run it claims it, beats while it runs and finishes it with
//! a derived result. Every failure is a `BacklogError`, never an empty answer.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// One row of the store's `intents` fact, as `intent_list` gives it. Only what a computer reads is kept.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentRow {
    pub id: String,
    pub kind: String,
    /// The command's own arguments.
    #[serde(default)]
    pub args: Value,
    /// The person the row was written for (`system` for the agent's own).
    #[serde(default)]
    pub issued_by: Option<String>,
    /// `requested`, `claimed`, `running`, then `done`, `refused`, `failed`, `timed_out` or `cancelled`.
    #[serde(default)]
    pub status: String,
}

impl IntentRow {
    /// Whether a computer may try to take it: new, or claimed or running (its own, before a restart; another's claim
    /// is lost at the store).
    pub fn open(&self) -> bool {
        matches!(self.status.as_str(), "requested" | "claimed" | "running")
    }
}

/// What `intent_claim` answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    #[serde(default)]
    pub won: bool,
    /// An older row ahead of it, or the board's queue, holds it back: wait and ask again; not a lost race.
    #[serde(default)]
    pub blocked: bool,
}

/// How a claimed row ended (`intent_finish`'s `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Done,
    /// The computer understood and said no.
    Refused,
    /// It broke or stopped.
    Failed,
}

impl Ending {
    fn word(self) -> &'static str {
        match self {
            Ending::Done => "done",
            Ending::Refused => "refused",
            Ending::Failed => "failed",
        }
    }
}

#[derive(Deserialize)]
struct Acked {
    #[serde(default)]
    ok: bool,
}

impl Client {
    /// `intent_list { kinds }`: the open rows and the finished ones still inside their hour, oldest first.
    pub async fn intent_list(
        &self,
        repo_id: u64,
        token: Option<&str>,
        kinds: &[&str],
    ) -> Result<Vec<IntentRow>, BacklogError> {
        self.op(repo_id, token, "intent_list", json!({ "kinds": kinds }), "intent rows")
            .await
    }

    /// `intent_claim { id, machine, can: true }`: this computer takes the row; the first claim wins.
    pub async fn intent_claim(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        machine: &str,
    ) -> Result<Claim, BacklogError> {
        let args = json!({ "id": id, "machine": machine, "can": true });
        self.op(repo_id, token, "intent_claim", args, "claim").await
    }

    /// `intent_beat { id }`: the holder is alive. `false` when the claim is gone (it went stale, or another holds it).
    pub async fn intent_beat(&self, repo_id: u64, token: Option<&str>, id: &str) -> Result<bool, BacklogError> {
        let ok: Acked = self
            .op(repo_id, token, "intent_beat", json!({ "id": id }), "beat")
            .await?;
        Ok(ok.ok)
    }

    /// `intent_finish { id, status, result?, why? }`. `false` when the claim was gone: the result recorded is another's.
    pub async fn intent_finish(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        ending: Ending,
        result: Option<&Value>,
        why: Option<&str>,
    ) -> Result<bool, BacklogError> {
        let mut args = json!({ "id": id, "status": ending.word() });
        if let Some(result) = result {
            args["result"] = result.clone();
        }
        if let Some(why) = why {
            args["why"] = json!(why);
        }
        let ok: Acked = self.op(repo_id, token, "intent_finish", args, "finish").await?;
        Ok(ok.ok)
    }

    /// A chat picture's bytes, `GET /v1/repos/<id>/pictures/<picture>` (last-local-state.md section 3.3).
    pub async fn picture(&self, repo_id: u64, token: Option<&str>, picture: &str) -> Result<Vec<u8>, BacklogError> {
        self.get_bytes(repo_id, token, &format!("pictures/{picture}")).await
    }
}

#[cfg(test)]
mod tests;
