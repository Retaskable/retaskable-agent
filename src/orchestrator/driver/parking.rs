//! An item waiting on the host or a person lets go of its lease and keeps its run (`parkFor`, `park`, `resume` in the TS agent's
//! `hostedWork.ts` and `hostedWorkHelpers.ts`; docs/specs/agent/07.5): in `awaiting_ci` (`ci`), `awaiting_playtest` (`tryout`) and
//! `awaiting_merge` where a person lands it (`merge`), with a change on the host and no lane held. The store keeps it as this
//! computer's, so no other computer is handed it, and its step goes on polling the host. When the wait ends here, the state moves on,
//! and the lease is claimed back by name before anything is written for it ([`ProjectLoop::claim_back`]). The park is kept in the
//! state file (`parked`), with the item's record, so a restart finds it again ([`ProjectLoop::restore_parked`]).
//!
//! Not parked, as in TS: `needs_human` with a session that worked on the item keeps its lease (Continue, Start over and Skip act
//! on it here); a synthetic chore item has no lease to let go of.
use super::{Links, ProjectLoop};
use crate::backlog::{Item, ItemStatus};
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::LanePool;
use crate::orchestrator::steps::state_name;
use crate::telemetry::{Event, event};

/// The item states by name, as the record keeps them.
fn state_of(name: &str) -> Option<ItemState> {
    use ItemState::*;
    Some(match name {
        "awaiting_ci" => AwaitingCi,
        "awaiting_playtest" => AwaitingPlaytest,
        "awaiting_merge" => AwaitingMerge,
        "merging" => Merging,
        "checking" => Checking,
        "reviewing" => Reviewing,
        _ => return None,
    })
}

/// What claiming a parked item back came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Claimed {
    Held,
    Gone,
    Later,
}

impl ProjectLoop {
    /// What the item would be parked waiting for, or `None` while it can't let go: it isn't waiting, it still holds a lane, it has
    /// no change on the host, it is a merge the host lands itself (a parked one showed Add it in on every board), or it is
    /// not leased here (a synthetic chore item has no store item to park).
    pub(super) fn park_for(&self, item: &str) -> Option<&'static str> {
        if !self.leases.contains_key(item) {
            return None;
        }
        let (run, record) = (self.runs.get(item)?, self.records.get(item));
        let has_change =
            record.is_some_and(|r| r.flow.pr_url.is_some()) || self.items.get(item).is_some_and(|i| i.pr_url.is_some());
        let lands_by_person = record.is_some_and(|r| r.flow.land.as_deref() == Some("person"));
        match run.state {
            ItemState::AwaitingCi => has_change.then_some("ci"),
            ItemState::AwaitingPlaytest => has_change.then_some("tryout"),
            ItemState::AwaitingMerge if lands_by_person => has_change.then_some("merge"),
            _ => None,
        }
    }

    /// Lets go of the lease while the item waits: its details go first while the lease is still held, then the lease is parked
    /// (kept owed until the store takes it) and the park is recorded.
    pub(super) async fn park_waiting(&mut self, pool: &mut LanePool, item: &str, links: &Links) {
        let Some(waits) = self.park_for(item) else { return };
        let state = self.runs.get(item).map_or("", |r| state_name(r.state));
        if let (Some(repo), Some(lease), Ok(Some(token))) =
            (self.repo, self.leases.get(item).cloned(), links.store_token().await)
            && let Some(held) = self.decisions.get_mut(item)
            && let Err(e) = held.flush(&self.client, repo, Some(&token), Some(&lease), item).await
        {
            // Kept: the details go with the lease still held next time, and nothing is lost for want of them now.
            self.store_error("details_report", &e);
            return;
        }
        self.parked.insert(item.to_string(), (waits.into(), state.into()));
        self.forget_progress(item);
        self.park(pool, item, state, waits, links).await;
        self.save_leases();
    }

    /// Claims back, by name, each parked item whose wait ended here (its state moved on from the one it was parked in), so it
    /// is this computer's to carry on. The store's refusal for good (another computer has it, it is gone) drops the run; one that
    /// could not be asked is tried again on the next pass. An item still waiting is left parked.
    pub(super) async fn claim_back(&mut self, links: &Links) {
        let ended: Vec<String> = (self.parked.iter())
            .filter(|(id, (_, was))| {
                self.runs.get(*id).is_none_or(|r| state_name(r.state) != was) && !self.unparked.contains_key(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            self.claim_one(&id, links).await;
        }
    }

    /// This computer as the store is told it when it claims an item by name or has one reopened for it.
    pub(super) fn claim_agent(&self, links: &Links) -> crate::backlog::ClaimNextAgent {
        crate::backlog::ClaimNextAgent {
            machine: links.machine.clone(),
            environment: links.environment.clone(),
            base_branch: self.project.base_branch.clone(),
            tags: links.tags.get(),
            week: self.week_use(),
        }
    }

    /// Claims `id` back by name now (it was parked). `Held` once the lease is this computer's again; `Gone` when the store
    /// refuses for good (another computer has it, it is gone) and the run is dropped; `Later` when the store couldn't be asked.
    pub(super) async fn claim_one(&mut self, id: &str, links: &Links) -> Claimed {
        if !self.runs.contains_key(id) {
            self.parked.remove(id);
            self.save_leases();
            return Claimed::Gone;
        }
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            return Claimed::Later;
        };
        let agent = self.claim_agent(links);
        match self.client.claim_task(repo, Some(&token), id, &agent).await {
            Ok(claimed) => {
                self.leases.insert(id.to_string(), claimed.token);
                self.parked.remove(id);
                self.forget_progress(id);
                self.save_leases();
                self.report_progress(links, id).await;
                self.events.add_in(
                    &self.id,
                    Event::Lease(event::Lease {
                        item: Some(id.to_string()),
                        change: Some("claimed".into()),
                        state: self.runs.get(id).map(|r| state_name(r.state).into()),
                        ..Default::default()
                    }),
                );
                Claimed::Held
            }
            Err(e) if matches!(e.code(), Some("conflict" | "not_found" | "held_by_owner")) => {
                tracing::info!(
                    "{}: {id} is no longer this computer's to claim back ({e}); dropped",
                    self.id
                );
                self.runs.remove(id);
                self.items.remove(id);
                self.records.remove(id);
                self.parked.remove(id);
                self.save_leases();
                Claimed::Gone
            }
            Err(e) => {
                self.store_error("claim", &e);
                Claimed::Later
            }
        }
    }

    /// After a restart: the items parked before it are taken up again, each as its record says, while the store still has them in a
    /// state that waits (another computer's take-over or a person's skip or merge has moved the rest on, and they are dropped).
    pub(super) fn restore_parked(&mut self, items: &[Item]) {
        for (id, (waits, was)) in std::mem::take(&mut self.parked) {
            let record = self.records.get(&id).cloned();
            let item = items.iter().find(|i| i.id == id);
            let waiting = |i: &Item| {
                matches!(
                    i.status,
                    ItemStatus::Claimed
                        | ItemStatus::Building
                        | ItemStatus::InReview
                        | ItemStatus::Merging
                        | ItemStatus::NeedsPerson
                )
            };
            let (Some(item), Some(record), Some(state)) = (item.filter(|i| waiting(i)), record, state_of(&was)) else {
                tracing::info!(
                    "{}: no longer holds {id}, which was parked for {waits}; dropped",
                    self.id
                );
                self.records.remove(&id);
                continue;
            };
            tracing::info!(
                "{}: still holds {id}, parked for {waits} in {was}, carrying on",
                self.id
            );
            let mut run = ItemRun::new();
            run.state = state;
            run.slice = item.slice;
            record.flow.restore_counters(&mut run);
            self.runs.insert(id.clone(), run);
            self.items.insert(id.clone(), item.clone());
            self.parked.insert(id, (waits, was));
        }
    }
}
