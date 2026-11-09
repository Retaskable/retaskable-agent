//! A person's answer to a playtest in the shared try-out queue (docs/specs/agent/06.4-playtests.md): an answer for
//! an item this computer isn't running is a try-out's, so it goes to the store with `tryout_result`. It is kept
//! owed (`unanswered`, in the state file) until the store takes it, and sent again on later passes by `flush_owed`,
//! so a person's answer is never lost. The store owns what follows a Problem.
use super::{Links, ProjectLoop};
use crate::backlog::TryoutOutcome;

impl ProjectLoop {
    /// Keeps a person's answer for `id` (a try-out's key, or the id of the item it is for) owed, then tries to send it.
    pub(super) async fn owe_answer(&mut self, id: &str, passed: bool, notes: Option<String>, links: &Links) {
        self.unanswered.insert(id.to_string(), (passed, notes));
        self.save_leases();
        self.flush_owed(links).await;
    }

    /// Sends the answers owed. One whose try-out isn't waiting is dropped and logged; one the store refuses or
    /// can't be reached for stays owed for the next pass.
    pub(super) async fn send_owed_answers(&mut self, repo: u64, token: &str) {
        if self.unanswered.is_empty() {
            return;
        }
        let waiting = match self.client.tryouts_list(repo, Some(token)).await {
            Ok(list) => list.waiting,
            Err(e) => {
                self.store_error("tryouts_list", &e);
                return;
            }
        };
        for (id, (passed, notes)) in self.unanswered.clone() {
            let Some(row) = waiting.iter().find(|t| t.key == id || t.item_id == id) else {
                tracing::info!("{}: dropped a playtest answer for {id}: no try-out is waiting", self.id);
                self.unanswered.remove(&id);
                continue;
            };
            let outcome = if passed {
                TryoutOutcome::Passed
            } else {
                TryoutOutcome::Problem
            };
            if let Err(e) = (self.client)
                .tryout_result(repo, Some(token), &row.key, outcome, notes.as_deref())
                .await
            {
                self.store_error("tryout_result", &e);
                continue;
            }
            self.unanswered.remove(&id);
        }
        self.save_leases();
    }
}

impl ProjectLoop {
    /// Remembers config.json's `playtests` (not a gated overlay's) and, when it has just become Off, owes the store
    /// its `release_playtests`. A first run with nothing remembered and Off already set owes nothing: the TS agent
    /// releases only on a change it sees, and with no earlier value there is no switch to see (what already waited
    /// is released on this computer by `release_playtests`, and by the TS agent's own pass).
    /// Returns whether a release is owed, so the caller can send it now rather than on the next flush.
    pub(super) fn note_playtests_switch(&mut self) -> bool {
        let now = self.file_project.playtests;
        if self.playtests_seen == Some(now) {
            return false;
        }
        use crate::config::Playtests::Off;
        if now == Off && self.playtests_seen.is_some() {
            self.release_owed = true;
        }
        self.playtests_seen = Some(now);
        self.save_leases();
        self.release_owed
    }

    /// Sends the owed `release_playtests`; cleared on success, kept owed (and logged) on a failure.
    pub(super) async fn send_owed_release(&mut self, repo: u64, token: &str) {
        if !self.release_owed {
            return;
        }
        match self.client.release_playtests(repo, Some(token)).await {
            Ok(()) => {
                self.release_owed = false;
                self.save_leases();
            }
            Err(e) => self.store_error("release_playtests", &e),
        }
    }
}
