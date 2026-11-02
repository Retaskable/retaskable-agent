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
