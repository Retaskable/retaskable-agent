//! The items a plan adds go to the store when it merges (docs/specs/agent/06.5-spec-drafts.md, "The plan's items";
//! Retaskable.com's backlog.md section 5, `sendDraftItems` in `agent/src/hostedBacklog.ts`). No spec carries an `Items:`
//! header for them: the drafter returns them in its result (`drafts::items`), and this computer keeps them with the plan
//! in `plan_items.pending.json` in the project's folder, listed before the plan's report is sent, so a restart between
//! "merged" and "added" loses nothing (TS: `itemsSent`, `markItemsSent`). A plan sent back and written again replaces
//! them; one that is discarded drops them.
//!
//! When the plan merges, by whichever path (the Approve on the lease the drafter holds, a person's Approve through
//! `draft_take`, or a pull request a person merged on the code host: `land.rs`), its entry is marked merged. Then, on every
//! pass, until the store has them: the ones of the plan it lacks are added (`add { via: "draft" }`, `todo`, with the
//! drafter's title, dependencies, spec refs, kind, milestone and checking call, and who asked), keeping only the
//! dependencies the store has or the same plan adds. An id already there is left as it is. A store that can't be asked
//! keeps the entry and nothing is sent (fail closed). The TS agent asks once per run and keeps the plan's record until
//! the store holds every item; here a failed add is also sent again after a restart because the entry is a file.
//!
//! What each answer means, as for a revision (`revise.rs`): taken is done; a refusal that sending again can't change
//! (`not_found`, `bad_request`, `forbidden`) is dropped with a `store_error`; anything else (unreachable, a 5xx, a
//! `conflict` from an id that arrived meanwhile) is kept for the next pass.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Shared;
use super::project::ProjectLoop;
use crate::drafts::items::PlanItem;
use crate::owed_file::{OwedFile, now_secs};

/// The file in the project's folder.
pub(super) const FILE: &str = "plan_items.pending.json";

/// A plan listed that never merged here (the store moved it to another computer, or it was lost) is forgotten after this long.
const UNMERGED_SECS: i64 = 30 * 24 * 60 * 60;

/// What a plan adds, by draft id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct PlanItems {
    pub items: Vec<PlanItem>,
    /// Who asked for the plan, for each item's `requestedBy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    /// The plan merged: its items are owed to the store.
    #[serde(default)]
    pub merged: bool,
    /// Epoch seconds, when it was first listed.
    pub since: i64,
}

pub(super) type Kept = OwedFile<PlanItems>;

pub(super) fn open(dir: &std::path::Path) -> Kept {
    OwedFile::open(dir.join(FILE))
}

/// Lists the items of a plan just written (or written again: the new ones replace the old, a plan that names none keeps the old).
pub(super) fn keep(kept: &Kept, draft_id: &str, items: Vec<PlanItem>, requested_by: Option<&str>) {
    if items.is_empty() {
        return;
    }
    let since = kept.get(draft_id).map_or_else(now_secs, |o| o.since);
    kept.put(
        draft_id,
        PlanItems {
            items,
            requested_by: requested_by.map(String::from),
            merged: false,
            since,
        },
    );
}

/// The plan merged: its listed items are now owed to the store. Done before the final report is sent, so a crash right after
/// the merge still sends them. `requested_by` is the store's word for who asked, when it has one.
pub(super) fn merged(kept: &Kept, draft_id: &str, requested_by: Option<&str>) {
    if let Some(owed) = kept.get(draft_id) {
        let requested_by = requested_by.map(String::from).or(owed.requested_by.clone());
        kept.put(
            draft_id,
            PlanItems {
                merged: true,
                requested_by,
                ..owed
            },
        );
    }
}

/// The items as `add` takes them: only the ones of `owed` the store lacks, each keeping the dependencies the store has or the
/// plan names.
fn added(
    owed: &PlanItems,
    have: &std::collections::BTreeSet<String>,
    known: &std::collections::BTreeSet<String>,
) -> Vec<Value> {
    let named: Vec<&str> = owed.items.iter().map(|i| i.id.as_str()).collect();
    (owed.items.iter())
        .filter(|i| !have.contains(&i.id))
        .map(|i| {
            let mut item = i.clone();
            item.depends_on
                .retain(|d| named.contains(&d.as_str()) || known.contains(d));
            let mut item = json!(item);
            if let Some(by) = &owed.requested_by {
                item["requestedBy"] = json!(by);
            }
            item
        })
        .collect()
}

impl ProjectLoop {
    /// Sends the items of each merged plan the store lacks. Nothing happens without the store's link or while stopping.
    pub(super) async fn send_plan_items(&mut self, shared: &Shared<'_>) {
        let now = now_secs();
        let mut owed: Vec<(String, PlanItems)> = self.plan_items.all().into_iter().collect();
        for (id, o) in &owed {
            if !o.merged && now - o.since > UNMERGED_SECS {
                self.plan_items.remove(id);
            }
        }
        owed.retain(|(_, o)| o.merged);
        owed.sort_by_key(|(_, o)| o.since);
        if owed.is_empty() || shared.stopping {
            return;
        }
        let Some((repo, token)) = self.decisions_link(shared).await else {
            return;
        };
        for (draft_id, o) in owed {
            match Self::add_plan_items(&self.client, repo, &token, &o).await {
                Ok(added) => {
                    if !added.is_empty() {
                        tracing::info!("{}: added {} from merged plan {draft_id}", self.id, added.join(", "));
                    }
                    self.plan_items.remove(&draft_id);
                }
                Err(e) if super::revise::definite(&e) => {
                    tracing::warn!("{}: couldn't add plan {draft_id}'s items: {e}", self.id);
                    self.store_error("add", &e);
                    self.plan_items.remove(&draft_id);
                }
                Err(e) => {
                    tracing::warn!("{}: adding plan {draft_id}'s items will be tried again: {e}", self.id);
                    self.store_error("add", &e);
                }
            }
        }
    }

    /// One plan: looks at which items the store has, then adds the rest. The ids added.
    async fn add_plan_items(
        client: &crate::backlog::Client,
        repo: u64,
        token: &str,
        o: &PlanItems,
    ) -> Result<Vec<String>, crate::backlog::BacklogError> {
        let token = Some(token);
        let ids: Vec<String> = o.items.iter().map(|i| i.id.clone()).collect();
        let have = client.items_named(repo, token, &ids).await?;
        let missing: Vec<&PlanItem> = o.items.iter().filter(|i| !have.contains(&i.id)).collect();
        if missing.is_empty() {
            return Ok(Vec::new());
        }
        // A dependency the plan doesn't name and the store doesn't have is left off.
        let outside: Vec<String> = {
            let mut outside: Vec<String> = (missing.iter())
                .flat_map(|i| i.depends_on.iter())
                .filter(|d| !ids.contains(d))
                .cloned()
                .collect();
            outside.sort();
            outside.dedup();
            outside
        };
        let known = match outside.is_empty() {
            true => Default::default(),
            false => client.items_named(repo, token, &outside).await?,
        };
        let items = added(o, &have, &known);
        let sent: Vec<String> = items
            .iter()
            .filter_map(|i| i["id"].as_str().map(String::from))
            .collect();
        client.add_plan_items(repo, token, items).await?;
        Ok(sent)
    }
}
