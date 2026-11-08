//! The items a project's loop holds a lease on, for the snapshot's `items`.
use super::project::ProjectLoop;
use crate::backlog::Item;
use crate::orchestrator::live::HeldItem;
use crate::orchestrator::session::branch_name;

impl ProjectLoop {
    /// The backlog's title, branch and PR link with the run record's state, lane and attempt. A lease whose
    /// item the backlog no longer lists is left out.
    pub(super) fn held_items(&self, items: &[Item]) -> Vec<HeldItem> {
        items
            .iter()
            .filter(|i| self.leases.contains_key(&i.id))
            .map(|i| {
                let record = self.records.get(&i.id);
                HeldItem {
                    id: i.id.clone(),
                    title: i.title.clone(),
                    state: record.map_or_else(|| "queued".to_string(), |r| r.state.clone()),
                    lane: record.and_then(|r| r.lane),
                    branch: Some(branch_name(i, self.attempt_of(&i.id))),
                    pr_url: i.pr_url.clone(),
                    attempt: self.attempt_of(&i.id),
                    chore: i.kind.as_deref() == Some("chore") || i.id.starts_with("chore-"),
                }
            })
            .collect()
    }
}
