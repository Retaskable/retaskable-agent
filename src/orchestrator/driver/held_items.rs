//! The items a project's loop holds, for the snapshot.

use super::project::ProjectLoop;

impl ProjectLoop {
    /// The items this loop holds a lease on, for the snapshot (`items` are the ones the store handed out).
    pub(super) fn held_items(&self) -> Vec<crate::orchestrator::live::HeldItem> {
        self.leases
            .keys()
            .map(|id| {
                let item = self.items.get(id);
                let record = self.records.get(id);
                crate::orchestrator::live::HeldItem {
                    id: id.clone(),
                    title: item.map(|i| i.title.clone()).unwrap_or_default(),
                    state: record.map(|r| r.state.clone()).unwrap_or_default(),
                    lane: record.and_then(|r| r.lane),
                    branch: record
                        .and_then(|r| r.branch.clone())
                        .or_else(|| item.and_then(|i| i.branch.clone())),
                    pr_url: item.and_then(|i| i.pr_url.clone()),
                    attempt: self.attempt_of(id),
                    chore: id.starts_with("chore-") || item.is_some_and(|i| i.kind.as_deref() == Some("chore")),
                }
            })
            .collect()
    }
}
