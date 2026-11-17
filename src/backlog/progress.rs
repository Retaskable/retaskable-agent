//! `report` as the TypeScript agent's `HostedWork.changed` sends it while an item is worked on (backlog.md section
//! 6.2): the item's status, the state it is in (`detail`), its branch and its change's address, with a person's
//! reason and whether Claude is on the stop. One of the store's own checks: only these five statuses are taken.
//! (`Client::report` is the other form, a plain note for the history.)
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// What an item's state says to the store (`reportStatus` in `hostedWorkHelpers.ts`).
pub const STATUSES: [&str; 5] = ["claimed", "building", "in_review", "merging", "needs_person"];

/// One progress report: `status` is one of [`STATUSES`], `detail` the agent's own state name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub status: String,
    pub detail: String,
    pub branch: String,
    pub pr_url: String,
    /// Why a person is needed (`needs_person` only), and whether Claude is looking at it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_on_it: Option<bool>,
}

impl Client {
    /// `report` with a status: the item's progress while its lease `lease` holds. A `stale_lease` refusal means
    /// another computer has it now.
    pub async fn report_progress(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        progress: &Progress,
    ) -> Result<(), BacklogError> {
        let mut args = serde_json::to_value(progress).unwrap_or(Value::Null);
        args["id"] = json!(id);
        args["token"] = json!(lease);
        self.send(repo_id, token, "report", args).await?;
        Ok(())
    }
}
