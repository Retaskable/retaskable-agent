//! A merged plan that revises an item puts that item back in the queue (docs/specs/agent/06.5-spec-drafts.md,
//! "Revisions"; the TS agent's `reviseAfterMerge`): the store's `revise`, keyed by the plan's id, so a repeat changes
//! nothing. The TS agent sends it once, after the merge, and only notes a failure on the plan's timeline. Here the call
//! is listed in the state file BEFORE it is sent (`Landing::revisions`) and sent again on every pass, also after a
//! restart, until the store answers: a plan that merged never leaves its item as it was because the store was away
//! for a minute (it differs from TS, which forgets a revision it couldn't send).
//!
//! What each answer means: an answer is done; a refusal that sending again can't change (`not_found`, `bad_request`,
//! `forbidden`: the item is gone, or the call was wrong) is dropped with a `store_error`; anything else (unreachable, a
//! 5xx, a refusal this agent doesn't know) is kept for the next pass.
use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::BacklogError;
use crate::backlog::revise::{Owed, Revised};

/// The refusals sending the same revision again can't change.
fn definite(e: &BacklogError) -> bool {
    matches!(e.code(), Some("not_found" | "bad_request" | "forbidden"))
}

impl ProjectLoop {
    /// Lists a merged plan's revision (saved before anything is sent). One already listed for the plan stays as it is.
    pub(super) fn owe_revision(&mut self, draft_id: &str, owed: Owed) {
        self.landing.revisions.entry(draft_id.to_string()).or_insert(owed);
        self.save_revisions();
    }

    fn save_revisions(&mut self) {
        self.store.set_revise_owed(&self.landing.revisions);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the revisions owed failed: {e:#}", self.id);
        }
    }

    /// Sends every listed revision.
    pub(super) async fn send_revisions(&mut self, shared: &Shared<'_>) {
        if self.landing.revisions.is_empty() || shared.stopping {
            return;
        }
        let Some((repo, token)) = self.decisions_link(shared) else {
            return;
        };
        for (draft_id, owed) in self.landing.revisions.clone() {
            let sent = self.client.revise(repo, Some(&token), &draft_id, &owed).await;
            match sent {
                Ok(done) => {
                    let line = match done {
                        Revised::Waiting => "takes the change once the slice in hand ends".to_string(),
                        Revised::Applied => "is back in the queue with the change".to_string(),
                        Revised::Repeat => "already has the change".to_string(),
                    };
                    tracing::info!("{}: plan {draft_id}: item {} {line}", self.id, owed.item);
                }
                Err(e) if definite(&e) => {
                    tracing::warn!("{}: couldn't revise item {} (plan {draft_id}): {e}", self.id, owed.item);
                    self.store_error("revise", &e);
                }
                Err(e) => {
                    let listed = self.now_secs() - owed.since;
                    tracing::warn!(
                        "{}: revising {} (plan {draft_id}) will be tried again, listed {listed}s: {e}",
                        self.id,
                        owed.item
                    );
                    self.store_error("revise", &e);
                    continue;
                }
            }
            self.landing.revisions.remove(&draft_id);
            self.save_revisions();
        }
    }
}
