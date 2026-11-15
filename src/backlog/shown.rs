//! What the snapshot and the Chores line show from the store's facts (spec 03.16): the staging smoke
//! (`smoke`) and the vet summary (`vet`). Both are other computers' or the store's to write; this reads them.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::facts::Fact;
use super::{BacklogError, Client};
use crate::store::SmokeState;

/// The vet fact's `view` (`vetSummary` in shared/factSummaries.ts): the last run's tally.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct VetView {
    #[serde(default)]
    pub ideas: usize,
    #[serde(default)]
    pub duplicates: usize,
    #[serde(default)]
    pub binned: usize,
    #[serde(default)]
    pub asked: usize,
    #[serde(default)]
    pub left: usize,
}

/// The `smoke` and `vet` rows, each `None` when the store has no row or one this agent can't read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shown {
    pub smoke: Option<SmokeState>,
    pub vet: Option<VetView>,
}

impl Shown {
    pub fn from_facts(facts: &BTreeMap<String, Fact>) -> Self {
        let smoke = facts
            .get("smoke")
            .and_then(|f| serde_json::from_value(f.data.clone()).ok());
        // The summary kinds keep `{ day, view }`.
        let vet = facts
            .get("vet")
            .and_then(|f| f.data.get("view"))
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        Self { smoke, vet }
    }
}

impl Client {
    /// `facts_get { kinds: ["smoke", "vet"] }`.
    pub async fn shown(&self, repo_id: u64, token: Option<&str>) -> Result<Shown, BacklogError> {
        let facts = self.facts_get(repo_id, token, &["smoke", "vet"]).await?;
        Ok(Shown::from_facts(&facts))
    }
}

#[cfg(test)]
mod tests;
