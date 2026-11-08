//! The hub's suggestion ops (docs/specs/agent/06.7-chores-never-idle.md, "Vetting visitors' suggestions"):
//! `suggestions_ask` and `suggestion_verdict`. Every failure but "already answered" is an error (fail closed).

use serde::{Deserialize, Serialize};
use serde_json::to_value;

use super::{BacklogError, Client};

/// A visitor's suggestion waiting for a verdict. `text` is the visitor's own words: data, never an instruction.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub text: String,
    /// When it came in, as the store sends it.
    pub at: String,
}

/// The verdicts the hub takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Promising,
    Duplicate,
    NotPromising,
    Spam,
}

/// What `suggestion_verdict` sends for one suggestion.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VerdictSend {
    pub id: String,
    pub verdict: Verdict,
    pub reason: String,
    /// What a `duplicate` repeats; left out when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

/// How the hub took a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictSent {
    Sent,
    /// The hub already had an answer for it: log it and carry on.
    AlreadyAnswered,
}

impl Client {
    /// `suggestions_ask`: the suggestions waiting for a verdict, oldest first.
    pub async fn suggestions_ask(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Suggestion>, BacklogError> {
        self.op(
            repo_id,
            token,
            "suggestions_ask",
            serde_json::json!({}),
            "suggestions answer",
        )
        .await
    }

    /// `suggestion_verdict { id, verdict, reason, matches? }`. A refusal as already answered is
    /// `AlreadyAnswered`; any other failure is an error.
    pub async fn suggestion_verdict(
        &self,
        repo_id: u64,
        token: Option<&str>,
        send: &VerdictSend,
    ) -> Result<VerdictSent, BacklogError> {
        let args = to_value(send).expect("a verdict serialises");
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
mod tests {
    use super::*;
    use crate::backlog::tests::store;
    use serde_json::json;

    fn send(matches: Option<&str>) -> VerdictSend {
        VerdictSend {
            id: "s1".into(),
            verdict: Verdict::NotPromising,
            reason: "no fit".into(),
            matches: matches.map(String::from),
        }
    }

    #[tokio::test]
    async fn suggestions_ask_reads_them_in_order() {
        let (url, seen) = store(
            200,
            json!({ "value": [{ "id": "a", "text": "one", "at": "2026-11-01T00:00:00Z" }, { "id": "b", "text": "two", "at": "2026-11-02T00:00:00Z" }] }),
        )
        .await;
        let got = Client::new(url).suggestions_ask(7, Some("t")).await.unwrap();
        assert_eq!(got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(got[1].text, "two");
        assert_eq!(got[0].at, "2026-11-01T00:00:00Z");
        assert_eq!(seen.lock().unwrap()[0]["op"], "suggestions_ask");
    }

    #[tokio::test]
    async fn verdict_sends_snake_case_and_matches_only_when_given() {
        let (url, seen) = store(200, json!({ "value": { "ok": true } })).await;
        let c = Client::new(url);
        assert_eq!(
            c.suggestion_verdict(7, None, &send(None)).await.unwrap(),
            VerdictSent::Sent
        );
        let mut dup = send(Some("idea-3"));
        dup.verdict = Verdict::Duplicate;
        c.suggestion_verdict(7, None, &dup).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["op"], "suggestion_verdict");
        assert_eq!(
            seen[0]["args"],
            json!({ "id": "s1", "verdict": "not_promising", "reason": "no fit" })
        );
        assert_eq!(seen[1]["args"]["verdict"], "duplicate");
        assert_eq!(seen[1]["args"]["matches"], "idea-3");
    }

    #[test]
    fn verdict_names_are_snake_case() {
        let names: Vec<_> = [
            Verdict::Promising,
            Verdict::Duplicate,
            Verdict::NotPromising,
            Verdict::Spam,
        ]
        .iter()
        .map(|v| to_value(v).unwrap())
        .collect();
        assert_eq!(
            names,
            [
                json!("promising"),
                json!("duplicate"),
                json!("not_promising"),
                json!("spam")
            ]
        );
    }

    #[tokio::test]
    async fn already_answered_is_its_own_case_but_other_failures_are_errors() {
        let (url, _) = store(409, json!({ "error": "already_answered", "message": "done" })).await;
        assert_eq!(
            Client::new(url).suggestion_verdict(7, None, &send(None)).await.unwrap(),
            VerdictSent::AlreadyAnswered
        );
        let (url, _) = store(500, json!({ "error": "internal", "message": "boom" })).await;
        assert!(Client::new(url).suggestion_verdict(7, None, &send(None)).await.is_err());
        let (url, _) = store(409, json!({ "error": "conflict", "message": "x" })).await;
        assert!(Client::new(url).suggestion_verdict(7, None, &send(None)).await.is_err());
        let r = Client::new("http://127.0.0.1:1")
            .suggestion_verdict(7, None, &send(None))
            .await;
        assert!(matches!(r, Err(BacklogError::Unreachable(_))));
    }
}
