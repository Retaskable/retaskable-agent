//! A plan this computer holds whose lease it no longer has: the store lists it held by this computer, waiting for a person,
//! but `held_drafts.json` has no lease for it (it was held before that file existed, or the file was lost). The store lets its
//! holder `draft_take` it again (the new lease makes the old one stale), so it is taken and let go at once, as the drafter
//! does with a plan that waits for a person.
//!
//! The agent's own id isn't known here (the store takes it from the link's token), so a row is this computer's when its
//! `heldBy` names this machine and environment; the store decides, and a `draft_take` it refuses (a store not yet upgraded
//! answers 409, another computer of the same name has the plan) changes nothing. The read and the takes are tried once
//! every [`EVERY`], whatever the answer, so a refusing store is not asked each pass.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::super::Shared;
use super::super::drafts::claim_agent;
use super::super::project::ProjectLoop;
use crate::backlog::Draft;

/// How long to wait before looking again.
const EVERY: Duration = Duration::from_secs(5 * 60);

/// When the next look is due, and whether a failure was already reported since the last good one.
#[derive(Clone, Default)]
pub(super) struct Look(Arc<Mutex<(Option<Instant>, bool)>>);

impl Look {
    fn due(&self) -> bool {
        let at = self.0.lock().unwrap_or_else(|e| e.into_inner()).0;
        at.is_none_or(|at| Instant::now() >= at)
    }

    /// Looked at: the next look is in [`EVERY`]. Returns whether this failure is the first since the last good look.
    fn looked(&self, failed: bool) -> bool {
        let mut look = self.0.lock().unwrap_or_else(|e| e.into_inner());
        look.0 = Some(Instant::now() + EVERY);
        let first = failed && !look.1;
        look.1 = failed;
        first
    }
}

/// Whether the row waits for a person with no word to say: a plan for approval, its design review or a release note.
fn let_go_state(row: &Draft) -> bool {
    row.status.as_deref() == Some("needs_person") && row.detail.as_deref() == Some("awaiting_approval")
}

/// Whether `held_by` names this machine and environment.
fn is_ours(held_by: Option<&Value>, machine: &str, environment: &str) -> bool {
    let text = |key: &str| held_by.and_then(|h| h.get(key)).and_then(Value::as_str);
    text("machine") == Some(machine) && text("environment") == Some(environment)
}

impl ProjectLoop {
    /// Takes back and lets go of the plans the store lists as held by this computer, waiting for a person, whose lease isn't
    /// listed, while no drafter or landing task works here. Fails closed: nothing changes when the store can't be asked or
    /// refuses.
    pub(in crate::orchestrator::driver) async fn free_own_held(&mut self, shared: &Shared<'_>) {
        if self.drafting() || !self.landing.tasks.is_empty() || !self.held.own.due() {
            return;
        }
        let Some(repo) = self.repo else { return };
        let Ok(Some(token)) = shared.links.store_token().await else {
            return;
        };
        let rows = match self.client.drafts(repo, Some(&token)).await {
            Ok(rows) => rows,
            Err(e) => {
                if self.held.own.looked(true) {
                    tracing::warn!(
                        "{}: couldn't read the drafts to find ones this computer holds: {e}",
                        self.id
                    );
                }
                return;
            }
        };
        let (machine, environment) = (&shared.links.machine, &shared.links.environment);
        let lost: Vec<String> = rows
            .iter()
            .filter(|r| let_go_state(r) && is_ours(r.held_by.as_ref(), machine, environment) && !self.held.holds(&r.id))
            .map(|r| r.id.clone())
            .collect();
        let agent = claim_agent(shared.links, &self.project);
        for id in lost {
            let taken = match self.client.draft_take(repo, Some(&token), &id, &agent).await {
                Ok(taken) => taken,
                Err(e) => {
                    if self.held.own.looked(true) {
                        tracing::warn!(
                            "{}: couldn't take back draft {id}, which this computer holds: {e}",
                            self.id
                        );
                        self.store_failure("draft_take", &id, &e).await;
                    }
                    // Fail closed: the rest would be refused the same way.
                    return;
                }
            };
            tracing::info!(
                "{}: letting go of draft {id}, which this computer held with no lease",
                self.id
            );
            // Listed first, so a release that doesn't get through is tried again by `free_held`.
            self.held.keep(&id, &taken.token);
            let held = self.held.clone();
            held.release(&self.client, repo, &token, &id, &taken.token, &self.failures)
                .await;
        }
        self.held.own.looked(false);
    }
}
