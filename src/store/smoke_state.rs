//! `State.smoke`: the post-merge smoke's outcome, kept in the project's state. [`crate::smoke`]
//! produces it and [`super::Store`] saves it, so it lives here and `store` depends on nothing above it.
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
