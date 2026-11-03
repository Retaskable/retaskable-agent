//! The store locks this computer holds for a project (docs/specs/pieces.md section 3.4), in memory only. A run's
//! worker takes and drops them through its [`RunLocks`] handle; the project's loop beats their tokens with the runs'.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::CEILING_MARGIN;
use crate::backlog::runs::BeatVerdict;

/// A lock's ceiling: 1 hour for a `merge-` lock, 6 hours otherwise.
pub fn lock_ceiling(name: &str) -> Duration {
    Duration::from_secs(if name.starts_with("merge-") { 3600 } else { 6 * 3600 })
}

struct Held {
    token: String,
    name: String,
    taken: Instant,
    lost: bool,
}

/// A project's held locks. Clones share one table.
#[derive(Clone, Default)]
pub struct RunLocks(Arc<Mutex<Vec<Held>>>);

impl std::fmt::Debug for RunLocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RunLocks({} held)", self.count())
    }
}

impl RunLocks {
    fn table(&self) -> std::sync::MutexGuard<'_, Vec<Held>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a lock the store just gave (`lock_take`'s token), so the loop beats it.
    pub fn add(&self, token: &str, name: &str, taken: Instant) {
        self.table().push(Held {
            token: token.into(),
            name: name.into(),
            taken,
            lost: false,
        });
    }

    /// Forgets a lock, after `lock_drop` (or once its holder saw it lost).
    pub fn remove(&self, token: &str) {
        self.table().retain(|l| l.token != token);
    }

    /// Whether the lock is still held: false once the store said `stop` or its ceiling is near, and for an unknown
    /// token. Its holder checks this before it acts.
    pub fn held(&self, token: &str) -> bool {
        self.table().iter().any(|l| l.token == token && !l.lost)
    }

    /// The tokens to beat: every lock not lost.
    pub fn tokens(&self) -> Vec<String> {
        self.table()
            .iter()
            .filter(|l| !l.lost)
            .map(|l| l.token.clone())
            .collect()
    }

    /// How many locks are still held (a lost one needs nothing said to the store).
    pub fn count(&self) -> usize {
        self.table().iter().filter(|l| !l.lost).count()
    }

    /// A `stop` for a lock's token marks that lock lost.
    pub fn apply_beat(&self, verdicts: &HashMap<String, BeatVerdict>) {
        for l in self.table().iter_mut() {
            if verdicts.get(&l.token) == Some(&BeatVerdict::Stop) {
                l.lost = true;
            }
        }
    }

    /// Marks lost every lock within [`CEILING_MARGIN`] of its ceiling.
    pub fn mark_past_ceilings(&self, now: Instant) {
        for l in self.table().iter_mut() {
            if now.saturating_duration_since(l.taken) >= lock_ceiling(&l.name).saturating_sub(CEILING_MARGIN) {
                l.lost = true;
            }
        }
    }
}
