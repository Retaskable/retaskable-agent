//! An item's decision facts (docs/specs/last-local-state/03.13): the counters, shas and notes the agent decides with,
//! kept on the store item's details record instead of in a local run record. This is the read side: `details_get`
//! and the typed view over the 26 decision fields. Writes are `details_report` patches built by [`Decisions::diff`].
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::complete::DetailsRecord;
use super::{BacklogError, Client};

/// `triages` as the store keeps it: how many times slice `slice` was triaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Triages {
    pub slice: u32,
    pub n: u32,
}

macro_rules! decisions {
    ($($field:ident : $key:literal : $ty:ty),* $(,)?) => {
        /// The decision fields of one item. `None` is "the store holds none" (a fresh item).
        #[derive(Debug, Clone, PartialEq, Default)]
        pub struct Decisions {
            $(pub $field: Option<$ty>,)*
        }

        impl Decisions {
            /// The decision fields present in a details record's `details`. A field of the wrong shape is read as
            /// absent: the store checks every patch, so this only happens for a record another version wrote.
            pub fn from_details(details: &Value) -> Self {
                Self {
                    $($field: details.get($key).and_then(|v| serde_json::from_value(v.clone()).ok()),)*
                }
            }

            /// Sets each field the patch names: a value sets it, `null` clears it. Other fields are left alone.
            pub fn apply(&mut self, patch: &Map<String, Value>) {
                $(
                    if let Some(v) = patch.get($key) {
                        self.$field = serde_json::from_value(v.clone()).ok();
                    }
                )*
            }

            /// The patch that turns `old` into `self`: changed fields only, a field gone as `null`.
            pub fn diff(&self, old: &Decisions) -> Map<String, Value> {
                let mut patch = Map::new();
                $(
                    if self.$field != old.$field {
                        patch.insert($key.into(), match &self.$field {
                            Some(v) => json!(v),
                            None => Value::Null,
                        });
                    }
                )*
                patch
            }
        }

        /// The store's names of the decision fields, in the order the spec lists them.
        pub const DECISION_FIELDS: &[&str] = &[$($key),*];
    };
}

decisions! {
    retry_notes: "retryNotes": String,
    reviewed_sha: "reviewedSha": String,
    approved_sha: "approvedSha": String,
    conflict_rounds: "conflictRounds": u32,
    check_rounds: "checkRounds": u32,
    reasks: "reasks": u32,
    triages: "triages": Triages,
    reported_bug: "reportedBug": String,
    attempt: "attempt": u32,
    slice_limit: "sliceLimit": u32,
    failed_in: "failedIn": String,
    merged_at: "mergedAt": String,
    follow_up: "followUp": String,
    conflict_of: "conflictOf": String,
    gh_failures: "ghFailures": u32,
    spawn_failures: "spawnFailures": u32,
    merge_retries: "mergeRetries": u32,
    options_tries: "optionsTries": u32,
    ci_reruns: "ciReruns": u32,
    ci_infra_sha: "ciInfraSha": String,
    checks_sha: "checksSha": String,
    ci_ready_at: "ciReadyAt": String,
    ci_wait_since: "ciWaitSince": String,
    baseline_wait_since: "baselineWaitSince": String,
    last_review: "lastReview": String,
    triaged: "triaged": bool,
}

impl Client {
    /// Reads item `id`'s details record: no lease needed. `None` when the store holds no record for it yet.
    pub async fn details_get(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
    ) -> Result<Option<DetailsRecord>, BacklogError> {
        let (status, body) = self.send(repo_id, token, "details_get", json!({ "id": id })).await?;
        let Some(value) = body.value.filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        serde_json::from_value(value)
            .map(Some)
            .map_err(|e| BacklogError::BadAnswer {
                status,
                code: None,
                message: format!("couldn't read the details record it sent: {e}"),
            })
    }
}

pub mod sync;

#[cfg(test)]
mod tests;
