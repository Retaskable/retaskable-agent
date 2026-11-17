//! `State.smoke`: the post-merge smoke's outcome, kept in the project's state. [`crate::smoke`]
//! produces it and [`super::Store`] saves it, so it lives here and `store` depends on nothing above it.
use serde::{Deserialize, Serialize};

/// What the fact may be as JSON, a little under the store's 8 KiB.
const FACT_ROOM: usize = 7 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeStatus {
    Waiting,
    Running,
    Passed,
    Failed,
    Timeout,
}

/// `State.smoke` (06.3-review.md: "kept in the project's state and reported to the hub as a
/// derived result").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmokeState {
    pub sha: String,
    pub status: SmokeStatus,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
}

impl SmokeState {
    /// Still mid-way when the agent restarted (`waiting` or `running`): the caller starts it
    /// again rather than leaving it as it stood (06.3-review.md: "A smoke cut off by a restart
    /// starts again").
    pub fn resumable(&self) -> bool {
        matches!(self.status, SmokeStatus::Waiting | SmokeStatus::Running)
    }

    /// The store's `smoke` fact: `{ sha, status, at, url?, tail? }` within its caps (the url 2000 characters, the tail 4000,
    /// its last ones, and the whole under the kind's 8 KiB as UTF-8 so the store never refuses it). As the TS agent sends it,
    /// only a failure carries its `tail`.
    pub fn to_fact(&self) -> serde_json::Value {
        let mut fact = serde_json::json!({ "sha": self.sha, "status": self.status, "at": self.at });
        if let Some(url) = &self.url {
            fact["url"] = url.chars().take(2000).collect::<String>().into();
        }
        if let (SmokeStatus::Failed, Some(tail)) = (self.status, &self.tail) {
            let mut keep = 4000;
            let mut cut = crate::text::tail(tail, keep);
            while fact.to_string().len() + cut.len() + 64 > FACT_ROOM && keep > 100 {
                keep -= 100;
                cut = crate::text::tail(tail, keep);
            }
            fact["tail"] = cut.into();
        }
        fact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_shape_uses_snake_case_status_and_skips_empty_optionals() {
        let state = SmokeState {
            sha: "abc".into(),
            status: SmokeStatus::Timeout,
            at: "2026-01-01T00:00:00Z".into(),
            url: None,
            tail: None,
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"sha": "abc", "status": "timeout", "at": "2026-01-01T00:00:00Z"})
        );
        assert_eq!(serde_json::from_value::<SmokeState>(json).unwrap(), state);
    }

    #[test]
    fn the_fact_keeps_a_failures_last_4000_characters_and_no_tail_for_a_pass() {
        let failed = SmokeState {
            sha: "abc".into(),
            status: SmokeStatus::Failed,
            at: "t".into(),
            url: Some("https://x.test".into()),
            tail: Some(format!("{}END", "a".repeat(5000))),
        };
        let fact = failed.to_fact();
        let tail = fact["tail"].as_str().unwrap();
        assert_eq!(tail.chars().count(), 4000);
        assert!(tail.ends_with("END"));
        assert_eq!(fact["url"], "https://x.test");
        let passed = SmokeState {
            status: SmokeStatus::Passed,
            ..failed
        };
        assert_eq!(passed.to_fact().get("tail"), None);
        assert_eq!(passed.to_fact()["status"], "passed");
    }

    #[test]
    fn a_tail_of_wide_characters_is_cut_to_fit_the_kinds_8_kib() {
        let state = SmokeState {
            sha: "abc".into(),
            status: SmokeStatus::Failed,
            at: "t".into(),
            url: Some("u".repeat(2000)),
            tail: Some("\u{4e2d}".repeat(4000)),
        };
        let fact = state.to_fact();
        assert!(fact.to_string().len() < 8 * 1024 - 1024, "{}", fact.to_string().len());
        assert!(!fact["tail"].as_str().unwrap().is_empty());
    }

    #[test]
    fn url_and_tail_round_trip_when_present() {
        let state = SmokeState {
            sha: "abc".into(),
            status: SmokeStatus::Failed,
            at: "t".into(),
            url: Some("https://x.test".into()),
            tail: Some("boom".into()),
        };
        let back: SmokeState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn an_older_file_without_url_or_tail_still_loads() {
        let state: SmokeState = serde_json::from_str(r#"{"sha":"s","status":"waiting","at":"t"}"#).unwrap();
        assert_eq!(state.status, SmokeStatus::Waiting);
        assert!(state.url.is_none() && state.tail.is_none());
    }
}
