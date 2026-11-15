//! An item's decision facts live on its store item (docs/specs/last-local-state/03.13, 03.15), not in the run record.
//! The loop reads them into `ProjectLoop::decisions` for every item it holds a lease on, edits them in memory as the
//! steps decide, and sends what changed as one `details_report` at the end of each pass. An item whose facts
//! couldn't be read (the store was down) has no entry, and what needs them waits: nothing is decided from a guess.
use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::decisions::Decisions;
use crate::backlog::decisions::sync::ItemDecisions;

impl ProjectLoop {
    /// Reads the decisions of every leased item not yet held, and lets go of those whose lease went.
    pub(super) async fn load_decisions(&mut self, shared: &Shared<'_>) {
        self.decisions.retain(|id, _| self.leases.contains_key(id));
        let missing: Vec<String> = (self.leases.keys())
            .filter(|id| !self.decisions.contains_key(*id))
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        let Some((repo, token)) = self.decisions_link(shared) else {
            return;
        };
        for id in missing {
            match ItemDecisions::load(&self.client, repo, Some(&token), &id).await {
                Ok(held) => {
                    self.decisions.insert(id, held);
                }
                Err(e) => {
                    self.store_error("details_get", &e);
                    return;
                }
            }
        }
    }

    /// Sends what changed in each item's decisions. One that the store didn't take stays changed and goes again next pass.
    pub(super) async fn flush_decisions(&mut self, shared: &Shared<'_>) {
        if !self.decisions.values().any(ItemDecisions::dirty) {
            return;
        }
        let Some((repo, token)) = self.decisions_link(shared) else {
            return;
        };
        let ids: Vec<String> = (self.decisions.iter())
            .filter(|(_, d)| d.dirty())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let lease = self.leases.get(&id).cloned();
            let Some(held) = self.decisions.get_mut(&id) else {
                continue;
            };
            if let Err(e) = held
                .flush(&self.client, repo, Some(&token), lease.as_deref(), &id)
                .await
            {
                self.store_error("details_report", &e);
            }
        }
    }

    /// The item's decisions, when they were read.
    pub(super) fn decisions_of(&self, id: &str) -> Option<&Decisions> {
        self.decisions.get(id).map(|d| &d.now)
    }

    fn decisions_link(&mut self, shared: &Shared<'_>) -> Option<(u64, String)> {
        let repo = self.repo?;
        match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => Some((repo, token)),
            Ok(None) => None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                None
            }
        }
    }
}
