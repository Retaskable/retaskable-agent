//! The leases this computer's drafters hold on plans that wait for a person (store-states.md section 3.6; the TS agent's
//! `draftSync.ts`: "Once it waits for approval ... it is let go (`draft_release`)").
//!
//! A plan the drafter just reported goes one of two ways, and the store's answer picks it:
//! - it waits for a person (no approve, a design review, a release note): the drafter lets go with `draft_release` once the
//!   report is in, so whichever computer the Approve, Discard or Plan-this press lands on takes it with `draft_take`;
//! - the store said approve: the lease is not let go. The Approve goes ahead on the lease the drafter holds ([`Held::approved`],
//!   taken by `land_drafts`), so no other computer can act on it in between, and `draft_take` (which refuses any held draft)
//!   is never asked for.
//!
//! The lease token is listed in `held_drafts.json` in the project's folder before the report is sent and dropped once it is let
//! go, so a restart between the report and the Approve (the press is queued in memory) can't strand the draft:
//! [`ProjectLoop::free_held`] lets go of every listed lease that no drafter, landing task or queued Approve is using. The
//! store drops the hold when a draft ends, so a lease listed for a plan that has since merged answers `stale_lease` and is dropped.
//!
//! A plan this computer holds whose lease was never listed (one written before the file existed, or lost on a restart) is
//! taken back and let go by [`ProjectLoop::free_own_held`] (`held_drafts/own.rs`).
mod own;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::Client;
use crate::backlog::drafts::TakenDraft;
use crate::backlog::failures::Failures;
use crate::owed_file::OwedFile;

/// The file in the project's folder: draft id to the lease this computer holds on it.
const FILE: &str = "held_drafts.json";

/// Clones share the listed leases and the approved plans.
#[derive(Clone)]
pub(super) struct Held {
    leases: OwedFile<String>,
    approved: Arc<Mutex<BTreeMap<String, TakenDraft>>>,
    /// When the store's list is next read for own holds with no lease listed (`own.rs`).
    own: own::Look,
}

impl Held {
    pub(super) fn open(dir: &Path) -> Self {
        Self {
            leases: OwedFile::open(dir.join(FILE)),
            approved: Default::default(),
            own: Default::default(),
        }
    }

    /// Lists a lease before the report that makes the plan wait for a person is sent.
    pub(super) fn keep(&self, id: &str, lease: &str) {
        self.leases.put(id, lease.to_string());
    }

    /// The store said approve: `taken` (the plan as it was just reported, with the lease) is carried out by the next land pass.
    pub(super) fn approved(&self, taken: TakenDraft) {
        let id = taken.draft.id.clone();
        self.approved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, taken);
    }

    /// The draft ended (merged): its hold went with it, so nothing is left to let go.
    pub(super) fn ended(&self, id: &str) {
        self.leases.remove(id);
    }

    /// The plan this computer holds for a press, if it does; the press then needs no `draft_take`.
    pub(super) fn take_approved(&self, id: &str) -> Option<TakenDraft> {
        self.approved.lock().unwrap_or_else(|e| e.into_inner()).remove(id)
    }

    /// Lets go of `id` (`draft_release`). The lease is forgotten when the store took it, or refused for a reason of its own
    /// (the draft moved on or ended: there is nothing left to let go); an unreachable store keeps it listed for the next pass.
    pub(super) async fn release(
        &self,
        client: &Client,
        repo: u64,
        token: &str,
        id: &str,
        lease: &str,
        failures: &Failures,
    ) {
        match client.draft_release(repo, Some(token), id, lease).await {
            Ok(()) => self.leases.remove(id),
            Err(e) => {
                tracing::info!("couldn't let go of draft {id}: {e}");
                if e.is_refusal() {
                    self.leases.remove(id);
                } else {
                    failures.add("draft_release", id, &e);
                }
            }
        }
    }

    /// Whether this computer knows the lease on `id`: listed in the file, or carried by a queued Approve.
    fn holds(&self, id: &str) -> bool {
        self.leases.get(id).is_some() || self.approved.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
    }

    /// Listed leases that no queued Approve holds.
    pub(super) fn stranded(&self) -> Vec<(String, String)> {
        let approved = self.approved.lock().unwrap_or_else(|e| e.into_inner());
        (self.leases.all().into_iter())
            .filter(|(id, _)| !approved.contains_key(id))
            .collect()
    }
}

impl ProjectLoop {
    /// Lets go of a lease this computer still holds on a plan that waits for a person while no drafter or landing task is
    /// working on it: the one a restart or a dropped Approve stranded. Nothing is asked of a store that can't be reached.
    pub(super) async fn free_held(&mut self, shared: &Shared<'_>) {
        if self.drafting() || !self.landing.tasks.is_empty() {
            return;
        }
        let stranded = self.held.stranded();
        if stranded.is_empty() {
            return;
        }
        let Some(repo) = self.repo else { return };
        let Ok(Some(token)) = shared.links.store_token().await else {
            return;
        };
        for (id, lease) in stranded {
            tracing::info!("{}: letting go of draft {id}, which waits for a person", self.id);
            let held = self.held.clone();
            held.release(&self.client, repo, &token, &id, &lease, &self.failures)
                .await;
        }
    }
}
