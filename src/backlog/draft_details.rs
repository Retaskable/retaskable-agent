//! A spec draft's details on the shared backlog, from the holder's side (`draft_details_get`, `draft_details_report`;
//! team-state-on-server.md section 3.3, TS `draftDetailsSync.ts`). The store keeps what the plan's page shows beside its status
//! (its request, questions, timeline, files, and, for a plan whose change people will see, `designReview`); the holder writes a
//! patch of only the changed fields, over the version it read: a stale version is refused with `conflict`.
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{BacklogError, Client};

/// A draft's details record: its version, and the fields (`designReview` is the review's JSON as text).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct DraftDetailsRecord {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub details: Map<String, Value>,
}

impl Client {
    /// `draft_details_get { id }`: the record, or none when the store holds no details for the draft yet (version 0).
    pub async fn draft_details_get(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
    ) -> Result<DraftDetailsRecord, BacklogError> {
        let (status, body) = self
            .send(repo_id, token, "draft_details_get", json!({ "id": id }))
            .await?;
        match body.value.filter(|v| !v.is_null()) {
            None => Ok(DraftDetailsRecord::default()),
            Some(v) => serde_json::from_value(v).map_err(|e| BacklogError::BadAnswer {
                status,
                code: None,
                message: format!("couldn't read the draft's details it sent: {e}"),
            }),
        }
    }

    /// `draft_details_report { id, token, baseVersion, patch }`: writes the changed fields of a draft this computer holds
    /// (`lease` is its token); a field set to `null` is cleared. The record as it is now.
    pub async fn draft_details_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        (id, lease): (&str, &str),
        base_version: u64,
        patch: &Map<String, Value>,
    ) -> Result<DraftDetailsRecord, BacklogError> {
        let args = json!({ "id": id, "token": lease, "baseVersion": base_version, "patch": patch });
        let (status, body) = self.send(repo_id, token, "draft_details_report", args).await?;
        match body.value.filter(|v| !v.is_null()) {
            None => Ok(DraftDetailsRecord {
                version: base_version + 1,
                details: patch.clone(),
            }),
            Some(v) => serde_json::from_value(v).map_err(|e| BacklogError::BadAnswer {
                status,
                code: None,
                message: format!("couldn't read the draft's details it sent: {e}"),
            }),
        }
    }
}
