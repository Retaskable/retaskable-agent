//! The hub's suggestion ops for the vetting chore (docs/specs/agent/06.7-chores-never-idle.md, "Vetting visitors'
//! suggestions"): `suggestions_ask` and `suggestion_verdict`. Every failure but an already-answered refusal is an error.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{BacklogError, Client};

/// The store's error code for a verdict on a suggestion it already has an answer for.
pub(super) const ALREADY_ANSWERED: &str = "already_answered";

/// A waiting suggestion from a visitor.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub text: String,
    /// When it came in, as the store sends it.
    pub at: String,
}

/// A verdict on a suggestion, as the store names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Promising,
    Duplicate,
    NotPromising,
    Spam,
}

/// What `suggestion_verdict` sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerdictSend {
    pub id: String,
    pub verdict: Verdict,
    pub reason: String,
    /// The waiting idea, built feature or spec a duplicate repeats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

/// How the store took a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictSent {
    Sent,
    /// The store already had an answer for the suggestion: log it and carry on.
    AlreadyAnswered,
}

impl Client {
    /// `suggestions_ask`: the suggestions waiting for a verdict, oldest first.
    pub async fn suggestions_ask(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Suggestion>, BacklogError> {
        self.op(repo_id, token, "suggestions_ask", json!({}), "suggestions")
            .await
    }

    /// `suggestion_verdict`: send one verdict. An already-answered refusal is `AlreadyAnswered`; any other
    /// failure is an error.
    pub async fn suggestion_verdict(
        &self,
        repo_id: u64,
        token: Option<&str>,
        verdict: &VerdictSend,
    ) -> Result<VerdictSent, BacklogError> {
        let args = serde_json::to_value(verdict).expect("a verdict serialises");
        match self
            .op::<serde_json::Value>(repo_id, token, "suggestion_verdict", args, "verdict answer")
            .await
        {
            Ok(_) => Ok(VerdictSent::Sent),
            Err(e) if e.is_already_answered() => Ok(VerdictSent::AlreadyAnswered),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests;
