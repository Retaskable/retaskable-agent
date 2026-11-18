//! An item this computer holds in the store but has no lease for here: the store lists it held by this computer, but
//! `leases` (state.json) has no token for it, because an older version's restart dropped it (`readopt`), the state file was
//! lost, or a claim's answer never got here. Nobody works on it and nothing else frees it while this computer is online.
//!
//! The store hands a holder its own lease back: `claim { id, agent }` on an item whose lease is held by the same agent
//! answers the token (Retaskable.com's `claim`; `ensureLease` in the TS agent relies on it), as `draft_take` does for a
//! plan (`held_drafts/own.rs`). So each such item is claimed by name and taken up again as `detail` says it was: a change
//! on the host waiting for CI or its merge resumes there; any other state starts again as queued, as a lease with no
//! saved run does in `readopt`.
//!
//! The agent's own id isn't known here (the store takes it from the link's token), so an item is this computer's when its
//! `claimedBy` names this machine and environment; the store decides, and a claim it refuses (another computer of the same
//! name holds it) changes nothing. Tried at most once every [`EVERY`], whatever the answer, and never before `readopt`.
use std::time::{Duration, Instant};

use super::Shared;
use super::drafts::claim_agent;
use super::project::ProjectLoop;
use crate::backlog::{Item, ItemStatus};
use crate::item_lifecycle::ItemState;

/// How long to wait before looking again.
const EVERY: Duration = Duration::from_secs(5 * 60);

/// When the next look is due.
#[derive(Default)]
pub(super) struct Look(Option<Instant>);

/// Whether `claimed_by` names this machine and environment.
fn is_ours(item: &Item, machine: &str, environment: &str) -> bool {
    let text = |key: &str| {
        item.claimed_by
            .as_ref()
            .and_then(|h| h.get(key))
            .and_then(|v| v.as_str())
    };
    text("machine") == Some(machine) && text("environment") == Some(environment)
}

/// The state a recovered item carries on in, from the holder's own word (`detail`, the state name it last reported).
fn state_from(item: &Item) -> ItemState {
    match item.detail.as_deref() {
        // Its change is on the host, which is where these steps look.
        Some("awaiting_ci") if item.pr_url.is_some() => ItemState::AwaitingCi,
        Some("merging") if item.pr_url.is_some() => ItemState::Merging,
        Some("awaiting_merge") if item.pr_url.is_some() => ItemState::AwaitingMerge,
        Some("awaiting_playtest") if item.pr_url.is_some() => ItemState::AwaitingPlaytest,
        Some("needs_human") => ItemState::NeedsHuman,
        // Anything with a session or a lane behind it is gone with the restart: it starts again.
        _ => ItemState::Queued,
    }
}

impl ProjectLoop {
    /// Claims back each item among `items` that the store lists as held by this computer while no lease is kept here, and takes
    /// it up. Fails closed: nothing changes when the store can't be asked or refuses.
    pub(super) async fn take_back_own(&mut self, items: &[Item], shared: &Shared<'_>) {
        if !self.adopted || shared.stopping || self.own_items.0.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let (machine, environment) = (&shared.links.machine, &shared.links.environment);
        let lost: Vec<&Item> = items
            .iter()
            .filter(|i| {
                i.lease.as_ref().is_some_and(|l| !l.is_null())
                    && matches!(
                        i.status,
                        ItemStatus::Claimed
                            | ItemStatus::Building
                            | ItemStatus::InReview
                            | ItemStatus::Merging
                            | ItemStatus::NeedsPerson
                    )
                    && is_ours(i, machine, environment)
                    && i.kind.as_deref() != Some("chore")
                    && !crate::expert::held_by_experts(&i.id, i.kind.as_deref(), i.detail.as_deref())
                    && !self.leases.contains_key(&i.id)
                    && !self.runs.contains_key(&i.id)
                    && !self.parked.contains_key(&i.id)
                    && !self.unreleased.contains_key(&i.id)
            })
            .collect();
        if lost.is_empty() {
            return;
        }
        let Some(repo) = self.repo else { return };
        let Ok(Some(token)) = shared.links.store_token().await else {
            return;
        };
        self.own_items.0 = Some(Instant::now() + EVERY);
        let agent = claim_agent(shared.links, &self.project);
        for listed in lost {
            let id = &listed.id;
            let claimed = match self.client.claim_held(repo, Some(&token), id, &agent).await {
                Ok(claimed) => claimed,
                Err(e) => {
                    tracing::warn!("{}: couldn't take back {id}, which this computer holds: {e}", self.id);
                    self.store_failure("claim", id, &e).await;
                    // Fail closed: the rest would be refused the same way.
                    return;
                }
            };
            let state = state_from(&claimed.item);
            tracing::info!(
                "{}: took back {id}, which this computer held with no lease ({})",
                self.id,
                crate::orchestrator::steps::state_name(state)
            );
            self.take_lease(&claimed.item, claimed.token, state);
            if state != ItemState::Queued {
                self.remember(id, None, None, None);
            }
            self.save_leases();
        }
    }
}
