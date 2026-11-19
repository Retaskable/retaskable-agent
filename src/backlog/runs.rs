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
    /// The engines this computer can run sessions on now (pieces.md section 3.6).
    pub engines: Vec<String>,
    /// The engine each kind's role runs on here.
    pub uses: RunTakeUses,
    /// `false` when the project has chores off, so the store hands out no rotation chore; left out otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chores: Option<bool>,
    /// This computer's Claude week; left out when it has no reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub week: Option<super::week::WeekUse>,
}

/// The store's count of rotation chores started today and its limit (`chores: { today, limit }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChoreCount {
    pub today: u32,
    pub limit: u32,
}

impl ChoreCount {
    /// The count in an answer's fields, or `None` when it is missing or malformed.
    pub fn read(fields: &Map<String, Value>) -> Option<Self> {
        let c = fields.get("chores")?;
        let num = |k: &str| c.get(k)?.as_u64().and_then(|n| u32::try_from(n).ok());
        Some(Self {
            today: num("today")?,
            limit: num("limit")?,
        })
    }
}

/// The engine each kind's role runs on: `plan` the planner, `build` and `fix` the implementer, `look` the triager.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RunTakeUses {
    pub plan: String,
    pub build: String,
    pub fix: String,
    pub look: String,
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
    /// The head a review approved, which a run may keep that approval for (pieces/03 "Approval reuse and tier escalation").
    #[serde(default)]
    pub approved_sha: Option<String>,
    /// On a fix sent only to merge the base in: the approved head it was sent from (equals `approved_sha`).
    #[serde(default)]
    pub conflict_of: Option<String>,
    /// The reasons the real diff held the piece for a check; a run can ignore it.
    #[serde(default)]
    pub held: Option<String>,
    /// Any other field of the piece.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl RunPiece {
    /// The piece's change as the store kept it: `change`, or else the `prUrl` / `prNumber` its piece record has
    /// (shared/pieces.ts names those two, and `pr_opened` writes them). `None` when it has no change at all.
    pub fn recorded_change(&self) -> Option<Value> {
        let set = |v: &Value| !v.is_null() && v.as_str().is_none_or(|s| !s.trim().is_empty());
        let named = |k: &str| self.other.get(k).filter(|v| set(v)).cloned();
        self.change
            .clone()
            .filter(set)
            .or_else(|| named("prUrl"))
            .or_else(|| named("prNumber"))
    }
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
    /// Another computer with more of its Claude week left takes the run; ask again after `retry_ms`.
    Passed(super::week::Passed),
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
    /// The session hit its engine's usage limit; `until` is when it lifts, if the engine said. Sent as
    /// `result.engine` and `result.until` (left out when unknown).
    Limited {
        engine: String,
        until: Option<chrono::DateTime<chrono::Utc>>,
    },
}

impl RunOutcome {
    /// The outcome as the state file keeps it, for an end the store hasn't taken ([`crate::runs::OwedEnd`]).
    pub fn to_json(&self) -> Value {
        match self {
            Self::Failed(why) => json!({ "outcome": "failed", "why": why }),
            Self::Limited { engine, until } => {
                json!({ "outcome": "limited", "engine": engine, "until": until.map(|t| t.to_rfc3339()) })
            }
            other => json!({ "outcome": other.wire() }),
        }
    }

    /// Reads [`Self::to_json`] back; `None` for an outcome this version doesn't know.
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(match v.get("outcome")?.as_str()? {
            "interrupted" => Self::Interrupted,
            "failed" => Self::Failed(v.get("why")?.as_str()?.to_string()),
            "planned" => Self::Planned,
            "pr_opened" => Self::PrOpened,
            "questions" => Self::Questions,
            "person_steps" => Self::PersonSteps,
            "nothing_left" => Self::NothingLeft,
            "green" => Self::Green,
            "red" => Self::Red,
            "merged" => Self::Merged,
            "updated" => Self::Updated,
            "conflict" => Self::Conflict,
            "looked" => Self::Looked,
            "limited" => Self::Limited {
                engine: v.get("engine")?.as_str()?.to_string(),
                until: v
                    .get("until")
                    .and_then(Value::as_str)
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&chrono::Utc)),
            },
            _ => return None,
        })
    }

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
            Self::Limited { .. } => "limited",
        }
    }
}

/// The most characters of a `failed` run's `why` the store takes (`str(result, "why", 300)` in `run_done`).
pub const WHY_MAX: usize = 300;

/// The `run_done` arguments for a run's end: the outcome's wire word, and the result with the fields the store reads
/// beside what the run made (`why` for `failed`, cut as the TypeScript agent's `failed` does; `engine` and `until`,
/// in epoch milliseconds, for `limited`).
pub fn run_done_args(run_token: &str, outcome: &RunOutcome, result: Value) -> Value {
    let mut result = result;
    let extra = match outcome {
        RunOutcome::Failed(why) => Some(json!({ "why": why.chars().take(WHY_MAX).collect::<String>() })),
        RunOutcome::Limited { engine, until } => {
            let mut o = json!({ "engine": engine });
            if let Some(until) = until {
                o["until"] = json!(until.timestamp_millis().max(0));
            }
            Some(o)
        }
        _ => None,
    };
    if let Some(Value::Object(extra)) = extra {
        match &mut result {
            Value::Object(o) => o.extend(extra),
            _ => result = Value::Object(extra),
        }
    }
    json!({ "token": run_token, "outcome": outcome.wire(), "result": result })
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

/// A run of a plan (or a look at one) has no piece: the store sends `piece: null` and the item as `item`
/// (`start` in `shared/backlogPieceOps.ts`: `id`, `title`, `specRefs`, `branch`, and `looks`, `engine` and `history`
/// when it has them). The item is read as the piece the run is on, `specRefs` as its `specs`, and `history` is
/// also kept on the run, where a look reads it. Any other answer is left as it is, so a malformed one still fails.
fn plan_brief(mut value: Value) -> Value {
    let Some(run) = value.as_object_mut() else {
        return value;
    };
    if !matches!(run.get("piece"), Some(Value::Null)) {
        return value;
    }
    let Some(Value::Object(mut item)) = run.get("item").cloned() else {
        return value;
    };
    if let Some(refs) = item.remove("specRefs") {
        item.entry("specs").or_insert(refs);
    }
    if let Some(history) = item.get("history") {
        run.insert("history".into(), history.clone());
    }
    run.insert("piece".into(), Value::Object(item));
    value
}

/// A piece has no `id` of its own: the store sends its number `n` and the item as `item` (`start` in
/// `shared/backlogPieceOps.ts`). The agent knows the piece by its record's id, `<item>-piece-<n>`
/// (`PieceRuns.recordId` in `pieceRuns.ts`), put in as the piece's `id`; the item's title goes in as `itemTitle`, for
/// the branch's name. A piece that has an `id` already is left as it is.
fn piece_record(mut value: Value) -> Value {
    let Some(run) = value.as_object_mut() else {
        return value;
    };
    let item = run.get("item").and_then(Value::as_object);
    let item_id = item
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let item_title = item.and_then(|i| i.get("title")).cloned();
    let Some(Value::Object(piece)) = run.get_mut("piece") else {
        return value;
    };
    if piece.contains_key("id") {
        return value;
    }
    if let (Some(item), Some(n)) = (item_id, piece.get("n").and_then(Value::as_u64)) {
        piece.insert("id".into(), Value::String(format!("{item}-piece-{n}")));
        if let Some(title) = item_title {
            piece.insert("itemTitle".into(), title);
        }
    }
    value
}

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
            Some(Value::Object(o)) if o.contains_key("passed") => Ok(RunTakeAnswer::Passed(
                super::week::Passed::of(&o).unwrap_or(super::week::Passed {
                    to: "another computer".into(),
                    retry_ms: 60_000,
                }),
            )),
            Some(value) => serde_json::from_value(piece_record(plan_brief(value)))
                .map(|run| RunTakeAnswer::Run(Box::new(run)))
                .map_err(|e| bad(format!("couldn't read the run it sent: {e}"))),
        }
    }

    /// `run_beat { tokens }`: each token's verdict. The store answers `{ runs: { <token>: "ok" | "stop" } }`
    /// (`shared/backlogPieceOps.ts`); a bare map of verdicts is read the same. `stop` means the piece was taken or
    /// changed, or the lock lost: the work is not this computer's any more. An unreachable store is an error, never a
    /// `stop`, so a failed beat cuts nothing.
    pub async fn run_beat(
        &self,
        repo_id: u64,
        token: Option<&str>,
        tokens: &[String],
    ) -> Result<HashMap<String, BeatVerdict>, BacklogError> {
        let answer: Value = self
            .op(repo_id, token, "run_beat", json!({ "tokens": tokens }), "beat verdicts")
            .await?;
        let verdicts = match answer {
            Value::Object(mut o) if matches!(o.get("runs"), Some(Value::Object(_))) => {
                o.remove("runs").unwrap_or_default()
            }
            other => other,
        };
        serde_json::from_value(verdicts).map_err(|e| BacklogError::BadAnswer {
            status: 200,
            code: None,
            message: format!("couldn't read the beat verdicts it sent: {e}"),
        })
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
        let args = run_done_args(run_token, outcome, result);
        match self.send(repo_id, token, "run_done", args).await {
            Ok(_) => Ok(()),
            Err(e) if e.code() == Some("stale_run") => Err(RunDoneError::Stale),
            Err(e) => Err(RunDoneError::Other(e)),
        }
    }

    /// `piece_answer { piece, answers }`: answers a piece waiting for its questions' answers, as a person's **Go with the
    /// recommended** does (`piece` is `<item>#<n>`, `answers` one line a question). The store readies the piece with the answers
    /// in its next brief; a piece not waiting for an answer is refused (`conflict`).
    pub async fn piece_answer(
        &self,
        repo_id: u64,
        token: Option<&str>,
        piece: &str,
        answers: &str,
    ) -> Result<(), BacklogError> {
        let args = json!({ "piece": piece, "answers": answers });
        self.send(repo_id, token, "piece_answer", args).await.map(|_| ())
    }

    /// `piece_note { piece, note }`: one line (1 to 500 characters) on the item's timeline as "Piece <n>: <note>". Only the agent that
    /// holds or last ran the piece may; `forbidden`, or an old store that doesn't know the op, is the caller's to log.
    pub async fn piece_note(
        &self,
        repo_id: u64,
        token: Option<&str>,
        piece: &str,
        note: &str,
    ) -> Result<(), BacklogError> {
        let args = json!({ "piece": piece, "note": note });
        self.send(repo_id, token, "piece_note", args).await.map(|_| ())
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

    /// `add { via: "deprecation", items: [removal item] }` (deprecate-feature.md section 3.1). An id already taken is
    /// a `conflict`.
    pub async fn add_removal(&self, repo_id: u64, token: Option<&str>, item: Value) -> Result<(), BacklogError> {
        self.send(repo_id, token, "add", json!({ "via": "deprecation", "items": [item] }))
            .await
            .map(|_| ())
    }

    /// `runs_drop`: ends every run and lock this agent had as `interrupted`.
    pub async fn runs_drop(&self, repo_id: u64, token: Option<&str>) -> Result<(), BacklogError> {
        self.send(repo_id, token, "runs_drop", json!({})).await.map(|_| ())
    }
}

/// A commit's first 7 characters.
pub fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

#[cfg(test)]
mod tests;
