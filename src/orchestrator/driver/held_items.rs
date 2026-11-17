//! The items a project's loop holds a lease on, as the snapshot's `items` reports them.
use super::project::ProjectLoop;
use crate::backlog::Item;
use crate::orchestrator::live::{HeldItem, HeldLane};
use crate::orchestrator::session::branch_name;

impl ProjectLoop {
    /// The leased items among `items` (the backlog as last read), with the lane each holds in `held`.
    pub(super) fn held_items(&self, items: &[Item], held: &[HeldLane]) -> Vec<HeldItem> {
        let mut out: Vec<HeldItem> = items
            .iter()
            .filter(|i| self.leases.contains_key(&i.id))
            .map(|i| HeldItem {
                id: i.id.clone(),
                title: i.title.clone(),
                state: serde_json::to_value(i.status)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                lane: held.iter().find(|h| h.item_id == i.id).map(|h| h.lane),
                branch: branch_name(i, self.attempt_of(&i.id)),
                pr_url: i.pr_url.clone(),
                attempt: self.attempt_of(&i.id),
                chore: i.kind.as_deref() == Some("chore")
                    || i.id.starts_with("chore-")
                    || crate::chore_items::item_kind(i).is_some(),
                expert: None,
            })
            .collect();
        // A chore item is not in the store's list: it is this computer's own.
        out.extend(self.chores.keys().filter_map(|id| {
            let (item, run) = (self.items.get(id)?, self.runs.get(id)?);
            Some(HeldItem {
                id: id.clone(),
                title: item.title.clone(),
                state: crate::orchestrator::steps::state_name(run.state).to_string(),
                lane: held.iter().find(|h| &h.item_id == id).map(|h| h.lane),
                branch: branch_name(item, 0),
                pr_url: self.records.get(id).and_then(|r| r.flow.pr_url.clone()),
                attempt: 0,
                chore: true,
                expert: None,
            })
        }));
        out.extend(self.expert_items());
        out
    }
}
