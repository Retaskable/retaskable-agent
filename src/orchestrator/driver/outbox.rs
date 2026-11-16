//! A completion the store hasn't taken yet (docs/specs/agent/07.5-what-a-claim-and-a-give-back-send.md; the TS agent's
//! `ItemRecord.outbox`, `hostedWork.finish`). `complete` is one write keyed by the lease, so the call is listed in
//! `ProjectLoop::outbox` and the state file BEFORE it is sent, and sent again on every pass, also after a restart,
//! until the store answers. It holds a call, never a decision: the item's decisions are sent first (the TS
//! `flushDetails`), and a flush the store didn't take keeps the call listed until it does.
//!
//! What each answer means:
//! - An answer is done: the call is removed. A repeat of a call the store has taken is answered as the first was, so a
//!   lost reply is just an answer here too.
//! - `stale_lease` (the item has moved on from this lease) and `conflict` (this lease completed with another merge
//!   commit) are definite: sending it again can't change them, so the entry is dropped with a `store_error` and a log
//!   line. The TS agent drops on `stale_lease` only (`leaseGone`); a `conflict` can't come from its outbox, which
//!   keeps one call per item, and here it is the same call sent again, so it is dropped the same way.
//! - Anything else (unreachable, a 5xx, a refusal this agent doesn't know) is kept for the next pass, for good, as the
//!   TS agent keeps it: only a definite refusal drops an entry. One that has been listed [`STUCK_RELEASE_SECS`] or
//!   more says so with an `error` event, and again once every such span while it stays stuck, not every pass.
use super::Shared;
use super::project::ProjectLoop;
use super::settle::STUCK_RELEASE_SECS;
use crate::backlog::BacklogError;
use crate::backlog::complete::{Completion, Pending};
use crate::telemetry::{Event, event};

/// What sending an item's listed completion came to.
pub(super) enum Sent {
    /// The store took it: the call that went (the first one listed for the lease, when one was already).
    Done(Completion),
    /// Not taken; still listed and sent again next pass. The reason, in words.
    Kept(String),
    /// Refused for good: no longer listed. The reason, in words.
    Dropped(String),
}

/// The refusals sending the same call again can't change.
fn definite(e: &BacklogError) -> bool {
    matches!(e.code(), Some("stale_lease" | "conflict"))
}

impl ProjectLoop {
    /// Lists `completion` for item `id` under `lease` (saved before anything is sent) and sends it. A call already
    /// listed for this lease stays as it is, so the store never hears two merge commits for one lease.
    pub(super) async fn complete_item(
        &mut self,
        repo: u64,
        token: &str,
        id: &str,
        lease: &str,
        completion: Completion,
    ) -> Sent {
        if self.outbox.get(id).is_none_or(|p| p.lease != lease) {
            let since = self.now_secs();
            let pending = Pending {
                lease: lease.into(),
                completion,
                since,
            };
            self.outbox.insert(id.to_string(), pending);
            self.save_leases();
        }
        self.send_pending(repo, token, id).await
    }

    /// Sends every listed completion, before anything else is done for those items.
    pub(super) async fn send_outbox(&mut self, shared: &Shared<'_>) {
        if self.outbox.is_empty() {
            return;
        }
        let Some((repo, token)) = self.decisions_link(shared).await else {
            return;
        };
        for id in self.outbox.keys().cloned().collect::<Vec<_>>() {
            match self.send_pending(repo, &token, &id).await {
                Sent::Done(_) => tracing::info!("{}: completed {id}, listed earlier", self.id),
                Sent::Kept(_) | Sent::Dropped(_) => {}
            }
        }
    }

    /// The decisions, then the call, for one listed item.
    async fn send_pending(&mut self, repo: u64, token: &str, id: &str) -> Sent {
        let Some(pending) = self.outbox.get(id).cloned() else {
            return Sent::Dropped("nothing is listed for it".into());
        };
        let flushed = match self.decisions.get_mut(id) {
            Some(held) => (held.flush(&self.client, repo, Some(token), Some(&pending.lease), id)).await,
            None => Ok(()),
        };
        let sent = match flushed {
            Ok(()) => self.sent_complete(repo, token, id, &pending).await,
            Err(e) => {
                self.store_error("details_report", &e);
                Err(e)
            }
        };
        match sent {
            Ok(()) => {
                self.outbox.remove(id);
                self.outbox_noted.remove(id);
                // The lease is spent: the item is built or back in the queue, and no longer ours to run.
                if self.leases.get(id) == Some(&pending.lease) {
                    self.runs.remove(id);
                    self.items.remove(id);
                    self.leases.remove(id);
                    self.records.remove(id);
                }
                self.save_leases();
                Sent::Done(pending.completion)
            }
            Err(e) if definite(&e) => {
                tracing::warn!("{}: completing {id} dropped: {e}", self.id);
                self.outbox.remove(id);
                self.outbox_noted.remove(id);
                self.save_leases();
                Sent::Dropped(e.to_string())
            }
            Err(e) => {
                let now = self.now_secs();
                let last = self.outbox_noted.get(id).copied().unwrap_or(pending.since);
                if now - last >= STUCK_RELEASE_SECS {
                    self.outbox_noted.insert(id.to_string(), now);
                    self.note_stuck_completion(id);
                }
                Sent::Kept(e.to_string())
            }
        }
    }

    /// `complete`, its failure noted as a `store_error` (the decisions' own failure was noted where it happened).
    async fn sent_complete(&mut self, repo: u64, token: &str, id: &str, pending: &Pending) -> Result<(), BacklogError> {
        match (self.client)
            .complete(repo, Some(token), id, &pending.lease, &pending.completion)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                self.store_error("complete", &e);
                Err(e)
            }
        }
    }

    fn note_stuck_completion(&mut self, id: &str) {
        tracing::error!(
            "{}: completing {id} has been stuck for 24 hours or more; still trying",
            self.id
        );
        self.events.add_in(
            &self.id,
            Event::Error(event::Error {
                item: Some(id.to_string()),
                r#where: Some("complete".into()),
                error: Some(
                    "the completion is still not taken by the store after 24 hours; it is kept and sent again".into(),
                ),
                ..Default::default()
            }),
        );
    }
}
