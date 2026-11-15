//! The store's `ci_result` and `log_add` ops (docs/specs/ci-watch-through-store.md section 3, item 1), a port of
//! Retaskable.com's `shared/backlogCiResult.ts`. A computer reads GitHub and sends the raw facts of a pull
//! request's head; the store decides the condition, counts the attempt and answers the one instruction to run
//! next, with the log entries and bell lines to write. The computer runs it and sends again with `did`. The
//! computer keeps no verdict of its own, so these are only the shapes and the two calls.

use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// The longest title sent.
const TITLE_MAX: usize = 200;
/// The longest `what`, and each log, sent (the store cuts to this too).
const WHAT_MAX: usize = 4000;
/// The most logs sent: the store refuses more.
const LOGS_MAX: usize = 10;

/// `s` within `max` characters.
fn cut(s: &str, max: usize) -> &str {
    s.char_indices().nth(max).map_or(s, |(i, _)| &s[..i])
}

fn title_cut<S: Serializer>(title: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(cut(title, TITLE_MAX))
}

fn what_cut<S: Serializer>(what: &str, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(cut(what, WHAT_MAX))
}

fn logs_cut<S: Serializer>(logs: &[Option<String>], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(
        logs.iter()
            .take(LOGS_MAX)
            .map(|l| l.as_deref().map(|l| cut(l, WHAT_MAX))),
    )
}

/// A head's checks as the host reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CiState {
    Pass,
    Fail,
    Pending,
}

/// What the store may say a red is: the change's own break, main moving under it, or the machine failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CiVerdict {
    Change,
    Expectation,
    NotChange,
}

/// Why a head is rerun: its failure was the machine's (`force`), or its check hung (`stuck`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CiRerun {
    Force,
    Stuck,
}

/// The checks of a head: `what` names the failed ones with log excerpts, `logs` are their logs (null where one
/// couldn't be read), `ended_at` is when the latest failed one ended (ms).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CiChecks {
    pub state: CiState,
    #[serde(serialize_with = "what_cut")]
    pub what: String,
    #[serde(serialize_with = "logs_cut")]
    pub logs: Vec<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<u64>,
    /// Not sent: no status and no check run at all were reported on the head (read as pending by the host; the pass
    /// calls it "no CI", a pass, once the project's CI grace has gone by).
    #[serde(skip)]
    pub nothing_reported: bool,
}

/// What a computer ran, said back on the next call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CiDid {
    /// A read-only session's verdict (null when it couldn't give one); `report` when it was report-only.
    Judge {
        verdict: Option<CiVerdict>,
        report: bool,
    },
    Fix {
        pushed: bool,
    },
    MergeMain {
        pushed: bool,
    },
    Rerun {
        rerun: CiRerun,
        ok: bool,
    },
}

/// The raw facts of a pull request's head, as a computer read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CiFacts {
    pub pr: u64,
    pub sha: String,
    #[serde(serialize_with = "title_cut")]
    pub title: String,
    pub branch: String,
    /// This computer may push to the branch (`may_push_to`).
    pub may_push: bool,
    /// The owner pressed Fix: the red path runs once whoever the author is.
    pub button: bool,
    pub conflicting: bool,
    pub checks: CiChecks,
    /// When this computer first saw the head pending (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_since: Option<u64>,
    /// Pull requests failing in this pass.
    pub red_now: u32,
    /// The base branch's sha, to tell whether main moved since a merge conflicted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did: Option<CiDid>,
}

/// What the store decided a head is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CiCondition {
    Red,
    Conflicted,
    Stuck,
    Green,
    Pending,
}

/// The one instruction to run next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "do", rename_all = "camelCase")]
pub enum CiNext {
    /// Read the failing log and say whose fault it is; with `report` it counts no attempt and pushes nothing.
    Judge {
        #[serde(default)]
        report: bool,
    },
    Fix,
    MergeMain,
    Rerun {
        rerun: CiRerun,
        /// The pending checks are cancelled first.
        #[serde(default)]
        hung: bool,
    },
    /// A `do` this agent doesn't know (a newer store's): nothing is run for the head, and the answer's logs and bells
    /// still are. (TS fails the whole answer on one.)
    #[serde(other)]
    Unknown,
}

/// A bell line, shown word for word.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CiBell {
    pub title: String,
    pub body: String,
}

/// The store's answer to `ci_result`. Fields it adds later are ignored.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CiAnswer {
    pub ok: bool,
    /// Why a Fix button press was refused.
    #[serde(default)]
    pub refused: Option<String>,
    pub condition: CiCondition,
    /// A red the machine caused, read from the logs.
    #[serde(default)]
    pub not_the_change: bool,
    /// A new finding on this head.
    #[serde(default)]
    pub found: bool,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub needs_person: bool,
    #[serde(default)]
    pub next: Option<CiNext>,
    /// Entries for the pull request's `ci` log (`log_add`).
    #[serde(default)]
    pub logs: Vec<Map<String, Value>>,
    #[serde(default)]
    pub bells: Vec<CiBell>,
}

impl Client {
    /// `ci_result` (ci-watch-through-store.md section 3, item 1): sends a head's facts, and what was run last,
    /// and answers the store's decision. A refused Fix press is `ok: false` with `refused`, not an error.
    pub async fn ci_result(
        &self,
        repo_id: u64,
        token: Option<&str>,
        facts: &CiFacts,
    ) -> Result<CiAnswer, BacklogError> {
        let args = serde_json::to_value(facts).map_err(|e| BacklogError::BadAnswer {
            status: 0,
            code: None,
            message: format!("couldn't write the facts: {e}"),
        })?;
        self.op(repo_id, token, "ci_result", args, "ci answer").await
    }

    /// `log_add`: one entry on a log of the project, `kind` `ci` for a pull request's (what `ci_result` words).
    pub async fn log_add(
        &self,
        repo_id: u64,
        token: Option<&str>,
        kind: &str,
        entry: &Map<String, Value>,
    ) -> Result<(), BacklogError> {
        self.op_done(repo_id, token, "log_add", json!({ "kind": kind, "entry": entry }))
            .await
    }
}

#[cfg(test)]
mod tests;
