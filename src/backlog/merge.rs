//! The store's merge queue ops (docs/specs/merge-queue-from-store.md section 3.1): the store decides whose turn it
//! is; this computer asks for the turn, beats it and reports how the work went. Every failure is an error.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::runs::{RunOutcome, RunPiece};
use super::{BacklogError, Client};

/// A turn the store handed out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeTurn {
    pub token: String,
    #[serde(default)]
    pub lease_ms: u64,
    #[serde(default)]
    pub entry: Value,
    /// The piece, for a piece's merge (`merge_next { piece: true }`): what `run_take` answers for a merge.
    #[serde(default)]
    pub piece: Option<RunPiece>,
    /// Any other field of the answer.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// What `merge_next` answers.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeNext {
    Turn(Box<MergeTurn>),
    /// Another change is ahead: its place in the queue and how many are ahead of it.
    Waiting {
        place: u64,
        ahead: u64,
    },
    /// None of this computer's changes is queued.
    Nothing,
}

/// A change to put in the queue (`merge_enqueue`).
#[derive(Debug, Clone, Default)]
pub struct MergeEntry<'a> {
    pub item: &'a str,
    pub piece: Option<&'a str>,
    pub base: &'a str,
    pub head: &'a str,
    pub approved_sha: Option<&'a str>,
    pub pr: &'a str,
    pub land: &'a str,
    /// A chore with no store item: queued by its id without looking the item up.
    pub chore: bool,
}

/// How the work of a turn went (`merge_result`'s `outcome`).
#[derive(Debug, Clone, PartialEq)]
pub enum MergeOutcome {
    Merged { merge_commit: Option<String> },
    Updated { head: String, approved_intact: bool },
    HandedOff,
    CiFailed { checks: Value },
    SentBack { reason: String },
    NeedsPerson { reason: String },
    Closed,
    Retry { after_seconds: u64 },
    Released,
}

impl MergeOutcome {
    /// What a finished merge run reports: a conflict is sent back, a failure or a question needs a person, and a
    /// run cut short gives the turn back with no verdict.
    pub fn of_run(outcome: &RunOutcome, result: &Value) -> Self {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
        match outcome {
            RunOutcome::Merged => Self::Merged {
                merge_commit: result.get("commit").and_then(Value::as_str).map(String::from),
            },
            RunOutcome::Updated => Self::Updated {
                head: text(result.get("head")),
                approved_intact: true,
            },
            RunOutcome::Conflict => Self::SentBack {
                reason: text(result.get("instruction")),
            },
            RunOutcome::Failed(why) => Self::NeedsPerson { reason: why.clone() },
            RunOutcome::Questions => Self::NeedsPerson {
                reason: text(result.pointer("/questions/0/question")),
            },
            _ => Self::Released,
        }
    }

    fn wire(&self) -> Value {
        match self {
            Self::Merged { merge_commit } => json!({ "outcome": "merged", "mergeCommit": merge_commit }),
            Self::Updated { head, approved_intact } => {
                json!({ "outcome": "updated", "head": head, "approvedIntact": approved_intact })
            }
            Self::HandedOff => json!({ "outcome": "handed_off" }),
            Self::CiFailed { checks } => json!({ "outcome": "ci_failed", "checks": checks }),
            Self::SentBack { reason } => json!({ "outcome": "sent_back", "reason": reason }),
            Self::NeedsPerson { reason } => json!({ "outcome": "needs_person", "reason": reason }),
            Self::Closed => json!({ "outcome": "closed" }),
            Self::Retry { after_seconds } => json!({ "outcome": "retry", "afterSeconds": after_seconds }),
            Self::Released => json!({ "outcome": "released" }),
        }
    }
}

/// What `merge_result` answers: whether the turn is still held, and what is next, if the store says.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MergeResultAnswer {
    pub held: bool,
    #[serde(default)]
    pub next: Option<Value>,
}

impl Client {
    /// `merge_enqueue`: `{ entry, place }`. A second call for the same change updates `head` and keeps its place.
    pub async fn merge_enqueue(
        &self,
        repo_id: u64,
        token: Option<&str>,
        e: &MergeEntry<'_>,
    ) -> Result<Value, BacklogError> {
        let mut args = json!({
            "item": e.item, "base": e.base, "head": e.head, "pr": e.pr, "land": e.land,
        });
        for (key, value) in [("piece", e.piece), ("approvedSha", e.approved_sha)] {
            if let Some(v) = value {
                args[key] = json!(v);
            }
        }
        if e.chore {
            args["chore"] = json!(true);
        }
        self.op(repo_id, token, "merge_enqueue", args, "queue entry").await
    }

    /// `merge_withdraw { item }`: the change left `merging` for another reason.
    pub async fn merge_withdraw(&self, repo_id: u64, token: Option<&str>, item: &str) -> Result<(), BacklogError> {
        self.send(repo_id, token, "merge_withdraw", json!({ "item": item }))
            .await
            .map(|_| ())
    }

    /// `merge_next { base, piece? }`: the turn, a place in the line, or `null`.
    pub async fn merge_next(
        &self,
        repo_id: u64,
        token: Option<&str>,
        base: &str,
        piece: bool,
    ) -> Result<MergeNext, BacklogError> {
        let mut args = json!({ "base": base });
        if piece {
            args["piece"] = json!(true);
        }
        let (status, body) = self.send(repo_id, token, "merge_next", args).await?;
        let bad = |message: String| BacklogError::BadAnswer {
            status,
            code: None,
            message,
        };
        match body.value {
            None | Some(Value::Null) => Ok(MergeNext::Nothing),
            Some(Value::Object(mut o)) => {
                if let Some(turn) = o.remove("turn") {
                    serde_json::from_value(turn)
                        .map(|t| MergeNext::Turn(Box::new(t)))
                        .map_err(|e| bad(format!("couldn't read the turn it sent: {e}")))
                } else if let Some(w) = o.get("waiting") {
                    let n = |k: &str| w.get(k).and_then(Value::as_u64);
                    match (n("place"), n("ahead")) {
                        (Some(place), Some(ahead)) => Ok(MergeNext::Waiting { place, ahead }),
                        _ => Err(bad("the waiting answer had no place and ahead".into())),
                    }
                } else {
                    Err(bad("the answer had neither a turn nor a place".into()))
                }
            }
            Some(_) => Err(bad("the answer was not an object".into())),
        }
    }

    /// `merge_beat { token, phase }`: whether the turn is still held. `false` means stop.
    pub async fn merge_beat(
        &self,
        repo_id: u64,
        token: Option<&str>,
        turn: &str,
        phase: &str,
    ) -> Result<bool, BacklogError> {
        #[derive(Deserialize)]
        struct Held {
            held: bool,
        }
        let args = json!({ "token": turn, "phase": phase });
        let held: Held = self.op(repo_id, token, "merge_beat", args, "beat answer").await?;
        Ok(held.held)
    }

    /// `merge_open {}`: this computer's held turn, if any.
    pub async fn merge_open(&self, repo_id: u64, token: Option<&str>) -> Result<Option<MergeTurn>, BacklogError> {
        #[derive(Deserialize)]
        struct Open {
            turn: Option<MergeTurn>,
        }
        let open: Open = self.op(repo_id, token, "merge_open", json!({}), "open turn").await?;
        Ok(open.turn)
    }

    /// `merge_result { token, outcome, … }`. A result from a former holder is dropped by the store: `held: false`.
    pub async fn merge_result(
        &self,
        repo_id: u64,
        token: Option<&str>,
        turn: &str,
        outcome: &MergeOutcome,
    ) -> Result<MergeResultAnswer, BacklogError> {
        let mut args = outcome.wire();
        args["token"] = json!(turn);
        self.op(repo_id, token, "merge_result", args, "result answer").await
    }
}

#[cfg(test)]
mod tests;
