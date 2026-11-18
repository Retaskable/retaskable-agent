//! The store's two ops a merged plan's items need (backlog.md section 5; `sendDraftItems` in Retaskable.com's
//! `agent/src/hostedBacklog.ts`): `list { ids }` to see which the store has, and `add { via: "draft", items }` for the rest.
use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{BacklogError, Client};

/// The most ids one `list { ids }` takes.
const IDS_MAX: usize = 100;

#[derive(Deserialize)]
struct Named {
    id: String,
}

impl Client {
    /// `list { ids }`: the ones of `ids` the store has. An error is "couldn't ask", never "none of them".
    pub async fn items_named(
        &self,
        repo_id: u64,
        token: Option<&str>,
        ids: &[String],
    ) -> Result<BTreeSet<String>, BacklogError> {
        let mut have = BTreeSet::new();
        for chunk in ids.chunks(IDS_MAX) {
            let found: Vec<Named> = self
                .op(repo_id, token, "list", json!({ "ids": chunk }), "items")
                .await?;
            have.extend(found.into_iter().map(|i| i.id));
        }
        Ok(have)
    }

    /// `add { via: "draft", items }`: the items a merged plan added, each `todo`. An id the store already has is a `conflict`.
    pub async fn add_plan_items(
        &self,
        repo_id: u64,
        token: Option<&str>,
        items: Vec<Value>,
    ) -> Result<(), BacklogError> {
        let args = json!({ "via": "draft", "items": items });
        self.op_done(repo_id, token, "add", args).await
    }
}
