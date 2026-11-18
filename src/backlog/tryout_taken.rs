//! The builder takes a problem the store forwarded for a try-out it sent (`tryout_taken`, `storeTryouts.problem`): the store
//! answers the rest of the row, so this computer keeps only `{ key, sessionId, lane }` of what it built, and records that
//! this machine is fixing it (so the store doesn't make a fix item of it after 2 minutes).
use serde::Deserialize;
use serde_json::json;

use super::{BacklogError, Client};

/// What `tryout_taken` answers. `taken: false` once the problem became a fix item in the store (the 2 minutes ran out) or
/// is another computer's: nothing is left to do here.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Taken {
    pub taken: bool,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub what: Option<String>,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub merged_at: Option<String>,
    /// Who found the problem.
    #[serde(default)]
    pub by: Option<String>,
}

impl Client {
    /// `tryout_taken { key, machine }`.
    pub async fn tryout_taken(
        &self,
        repo_id: u64,
        token: Option<&str>,
        key: &str,
        machine: &str,
    ) -> Result<Taken, BacklogError> {
        self.op(
            repo_id,
            token,
            "tryout_taken",
            json!({ "key": key, "machine": machine }),
            "try-out taken answer",
        )
        .await
    }
}
