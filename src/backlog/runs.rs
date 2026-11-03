//! The store's run ops (docs/specs/pieces.md section 3.1): `run_take`, `run_beat`, `run_done` and
//! `runs_drop`, sent like the other ops. Every failure is an error, never an empty answer.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// The kinds of run (pieces.md section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Plan,
    Build,
    Fix,
    Watch,
    Merge,
    Look,
}

/// What `run_take` sends about this computer: the same form as `ClaimNextAgent`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTakeAgent {
    pub machine: String,
    pub environment: String,
    pub base_branch: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// What `run_take` asks for: any run of some kinds, or one piece by name.
#[derive(Debug, Clone)]
pub enum RunWant<'a> {
    Kinds(&'a [RunKind]),
    Piece(&'a str),
}

/// The piece a run is on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPiece {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub what: String,
    #[serde(default)]
    pub done_when: String,
    #[serde(default)]
    pub attempts: Vec<Value>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub change: Option<Value>,
    /// Any other field of the piece.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// A run the store handed out.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Run {
    pub token: String,
    pub kind: RunKind,
    pub piece: RunPiece,
    /// Any other brief field.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// What `run_take` answers.
#[derive(Debug, Clone, PartialEq)]
pub enum RunTakeAnswer {
    Run(Box<Run>),
    /// A tag this computer lacks.
    WaitingFor(Value),
    Nothing,
}

/// The store's verdict on one token in `run_beat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeatVerdict {
    Ok,
    Stop,
}

/// How a run ended (section 3.2). `Failed` carries why; it is sent as `result.why`.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Interrupted,
    Failed(String),
    Planned,
    PrOpened,
    Questions,
    PersonSteps,
    NothingLeft,
    Green,
    Red,
    Merged,
    Updated,
    Conflict,
    Looked,
}

impl RunOutcome {
    fn wire(&self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Failed(_) => "failed",
            Self::Planned => "planned",
            Self::PrOpened => "pr_opened",
            Self::Questions => "questions",
            Self::PersonSteps => "person_steps",
            Self::NothingLeft => "nothing_left",
            Self::Green => "green",
            Self::Red => "red",
            Self::Merged => "merged",
            Self::Updated => "updated",
            Self::Conflict => "conflict",
            Self::Looked => "looked",
        }
    }
}

/// What `lock_take` answers: its token, or who else holds the lock.
#[derive(Debug, Clone, PartialEq)]
pub enum LockTake {
    Held(String),
    HeldBy { by: String, note: Option<String> },
}

/// Why `run_done` didn't take: the run is gone (throw the work away), or any other failure.
#[derive(Debug)]
pub enum RunDoneError {
    Stale,
    Other(BacklogError),
}

impl std::fmt::Display for RunDoneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale => write!(f, "the run is stale"),
            Self::Other(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RunDoneError {}

impl Client {
    /// `run_take { agent, kinds }` or `{ agent, piece }`. A `null` value is `Nothing`.
    pub async fn run_take(
        &self,
        repo_id: u64,
        token: Option<&str>,
        agent: &RunTakeAgent,
        want: RunWant<'_>,
    ) -> Result<RunTakeAnswer, BacklogError> {
        let args = match want {
            RunWant::Kinds(kinds) => json!({ "agent": agent, "kinds": kinds }),
            RunWant::Piece(piece) => json!({ "agent": agent, "piece": piece }),
        };
        let (status, body) = self.send(repo_id, token, "run_take", args).await?;
        let bad = |message: String| BacklogError::BadAnswer {
            status,
            code: None,
            message,
        };
        match body.value {
            None | Some(Value::Null) => Ok(RunTakeAnswer::Nothing),
            Some(Value::Object(mut o)) if o.contains_key("waitingFor") => {
                Ok(RunTakeAnswer::WaitingFor(o.remove("waitingFor").unwrap_or_default()))
            }
            Some(value) => serde_json::from_value(value)
                .map(|run| RunTakeAnswer::Run(Box::new(run)))
                .map_err(|e| bad(format!("couldn't read the run it sent: {e}"))),
        }
    }

    /// `run_beat { tokens }`: each token's verdict.
    pub async fn run_beat(
        &self,
        repo_id: u64,
        token: Option<&str>,
        tokens: &[String],
    ) -> Result<HashMap<String, BeatVerdict>, BacklogError> {
        self.op(repo_id, token, "run_beat", json!({ "tokens": tokens }), "beat verdicts")
            .await
    }

    /// `run_done { token, outcome, result }`. A `stale_run` refusal is `RunDoneError::Stale`.
    pub async fn run_done(
        &self,
        repo_id: u64,
        token: Option<&str>,
        run_token: &str,
        outcome: &RunOutcome,
        result: Value,
    ) -> Result<(), RunDoneError> {
        let mut result = result;
        if let RunOutcome::Failed(why) = outcome {
            match &mut result {
                Value::Object(o) => {
                    o.insert("why".into(), json!(why));
                }
                _ => result = json!({ "why": why }),
            }
        }
        let args = json!({ "token": run_token, "outcome": outcome.wire(), "result": result });
        match self.send(repo_id, token, "run_done", args).await {
            Ok(_) => Ok(()),
            Err(e) if e.code() == Some("stale_run") => Err(RunDoneError::Stale),
            Err(e) => Err(RunDoneError::Other(e)),
        }
    }

    /// `lock_take { name, note? }` (pieces.md section 3.4): `{ held: true, token }` or `{ held: false, by, note? }`.
    pub async fn lock_take(
        &self,
        repo_id: u64,
        token: Option<&str>,
        name: &str,
        note: Option<&str>,
    ) -> Result<LockTake, BacklogError> {
        #[derive(Deserialize)]
        struct Answer {
            held: bool,
            token: Option<String>,
            by: Option<String>,
            note: Option<String>,
        }
        let mut args = json!({ "name": name });
        if let Some(note) = note {
            args["note"] = json!(note);
        }
        let (status, body) = self.send(repo_id, token, "lock_take", args).await?;
        let bad = |message: &str| BacklogError::BadAnswer {
            status,
            code: None,
            message: message.into(),
        };
        let value = body.value.ok_or_else(|| bad("its answer had no value"))?;
        let a: Answer =
            serde_json::from_value(value).map_err(|e| bad(&format!("couldn't read the lock answer: {e}")))?;
        match (a.held, a.token, a.by) {
            (true, Some(token), _) => Ok(LockTake::Held(token)),
            (false, _, Some(by)) => Ok(LockTake::HeldBy { by, note: a.note }),
            _ => Err(bad("the lock answer had neither a token nor a holder")),
        }
    }

    /// `lock_drop { token }`: lets a lock go.
    pub async fn lock_drop(&self, repo_id: u64, token: Option<&str>, lock_token: &str) -> Result<(), BacklogError> {
        self.send(repo_id, token, "lock_drop", json!({ "token": lock_token }))
            .await
            .map(|_| ())
    }

    /// `runs_drop`: ends every run and lock this agent had as `interrupted`.
    pub async fn runs_drop(&self, repo_id: u64, token: Option<&str>) -> Result<(), BacklogError> {
        self.send(repo_id, token, "runs_drop", json!({})).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests;
