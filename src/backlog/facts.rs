//! `facts_get`: the project's facts, one row per kind (spec 03.16). The store answers a map of
//! `{ kind: { kind, data, updatedAt, by, machine? } }`, leaving out a kind with no row.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// One fact row.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    #[serde(default)]
    pub data: Value,
    /// When the row was last written (milliseconds); a writer sends it back as `ifUpdatedAt`.
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub machine: Option<String>,
}

impl Client {
    /// `facts_get { kinds }`: the rows that exist, by kind. A kind with no row is left out of the answer.
    pub async fn facts_get(
        &self,
        repo_id: u64,
        token: Option<&str>,
        kinds: &[&str],
    ) -> Result<BTreeMap<String, Fact>, BacklogError> {
        self.op(repo_id, token, "facts_get", json!({ "kinds": kinds }), "facts")
            .await
    }
}

#[cfg(test)]
mod tests;
