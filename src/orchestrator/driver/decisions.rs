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
        let Some((repo, token)) = self.decisions_link(shared).await else {
            return;
        };
        for id in missing {
            if !self.load_decisions_of(repo, &token, id).await {
                return;
            }
        }
    }

    /// Reads one item's decisions now (an item claimed this pass has none yet). False when the store didn't answer.
    pub(super) async fn ensure_decisions(&mut self, id: &str, shared: &Shared<'_>) -> bool {
        // A chore item has no store item, so no facts to read.
        if self.decisions.contains_key(id) || self.chores.contains_key(id) {
            return true;
        }
        let Some((repo, token)) = self.decisions_link(shared).await else {
            return false;
        };
        self.load_decisions_of(repo, &token, id.to_string()).await
    }

    async fn load_decisions_of(&mut self, repo: u64, token: &str, id: String) -> bool {
        match ItemDecisions::load(&self.client, repo, Some(token), &id).await {
            Ok(held) => {
                self.decisions.insert(id, held);
                true
            }
            Err(e) => {
                self.store_error("details_get", &e);
                false
            }
        }
    }

    /// Sends what changed in each item's decisions. One that the store didn't take stays changed and goes again next pass.
    pub(super) async fn flush_decisions(&mut self, shared: &Shared<'_>) {
        if !self.decisions.values().any(ItemDecisions::dirty) {
            return;
        }
        let Some((repo, token)) = self.decisions_link(shared).await else {
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

    pub(super) async fn decisions_link(&mut self, shared: &Shared<'_>) -> Option<(u64, String)> {
        let repo = self.repo?;
        match shared.links.store_token().await {
            Ok(Some(token)) => Some((repo, token)),
            Ok(None) => None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                None
            }
        }
    }
}
