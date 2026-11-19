//! The store locks this computer holds for a project (docs/specs/pieces.md section 3.4), in memory only. A run's
//! worker takes and drops them through its [`RunLocks`] handle; the project's loop beats their tokens with the runs'.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::backlog::runs::BeatVerdict;

/// A merge turn's phase from taking it until it lands (the TypeScript agent's background beat says the same).
pub const UPDATING: &str = "updating";
/// A merge turn's phase from the beat just before `gh pr merge`.
pub const MERGING: &str = "merging";
/// A merge turn's phase while GitHub's auto-merge is on and the turn waits for it.
pub const AUTO_MERGE: &str = "auto_merge";

struct Held {
    token: String,
    lost: bool,
    /// A merge queue turn: beaten with `merge_beat`, not `run_beat`.
    turn: bool,
    /// What a merge turn is doing, for `merge_beat`'s `phase`: `updating`, then `merging` at the host call.
    phase: &'static str,
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
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a lock the store just gave (`lock_take`'s token), so the loop beats it.
    pub fn add(&self, token: &str) {
        self.table().push(Held {
            token: token.into(),
            lost: false,
            turn: false,
            phase: UPDATING,
        });
    }

    /// Records a merge turn the store just gave (`merge_next`'s token): the loop beats it with `merge_beat`.
    pub fn add_turn(&self, token: &str) {
        self.add(token);
        if let Some(l) = self.table().last_mut() {
            l.turn = true;
        }
    }

    /// Records what a merge turn is doing now, for the loop's beats.
    pub fn set_phase(&self, token: &str, phase: &'static str) {
        if let Some(l) = self.table().iter_mut().find(|l| l.token == token) {
            l.phase = phase;
        }
    }

    /// What a merge turn is doing: `updating` for one the table doesn't know.
    pub fn phase(&self, token: &str) -> &'static str {
        self.table()
            .iter()
            .find(|l| l.token == token)
            .map_or(UPDATING, |l| l.phase)
    }

    /// Forgets a lock, after `lock_drop` (or once its holder saw it lost).
    pub fn remove(&self, token: &str) {
        self.table().retain(|l| l.token != token);
    }

    /// Whether the lock is still held: false once the store said `stop` (its deadline is the store's, `lockDeadline`), and for an unknown
    /// token. Its holder checks this before it acts.
    pub fn held(&self, token: &str) -> bool {
        self.table().iter().any(|l| l.token == token && !l.lost)
    }

    /// The tokens to beat with `run_beat`: every lock not lost, except merge turns.
    pub fn tokens(&self) -> Vec<String> {
        self.table()
            .iter()
            .filter(|l| !l.lost && !l.turn)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdicts(pairs: &[(&str, BeatVerdict)]) -> HashMap<String, BeatVerdict> {
        pairs.iter().map(|(t, v)| (t.to_string(), *v)).collect()
    }

    #[test]
    fn add_then_remove_tracks_tokens_and_count() {
        let locks = RunLocks::default();
        locks.add("t1");
        locks.add("t2");
        assert_eq!(locks.count(), 2);
        assert_eq!(locks.tokens(), vec!["t1".to_string(), "t2".to_string()]);
        assert!(locks.held("t1"));
        locks.remove("t1");
        assert!(!locks.held("t1"));
        assert_eq!(locks.tokens(), vec!["t2".to_string()]);
    }

    #[test]
    fn a_merge_turn_is_held_but_not_beaten_with_the_runs() {
        let locks = RunLocks::default();
        locks.add_turn("T");
        assert!(locks.held("T"));
        assert!(locks.tokens().is_empty());
        assert_eq!(locks.count(), 1);
        locks.apply_beat(&verdicts(&[("T", BeatVerdict::Stop)]));
        assert!(!locks.held("T"));
    }

    #[test]
    fn a_turns_phase_is_updating_until_it_says_merging() {
        let locks = RunLocks::default();
        locks.add_turn("T");
        assert_eq!(locks.phase("T"), "updating");
        locks.set_phase("T", MERGING);
        assert_eq!(locks.phase("T"), "merging");
        assert_eq!(locks.phase("unknown"), "updating");
    }

    #[test]
    fn an_unknown_token_is_not_held() {
        assert!(!RunLocks::default().held("nope"));
    }

    #[test]
    fn a_stop_verdict_marks_only_that_lock_lost() {
        let locks = RunLocks::default();
        locks.add("t1");
        locks.add("t2");
        locks.apply_beat(&verdicts(&[("t1", BeatVerdict::Stop), ("t2", BeatVerdict::Ok)]));
        assert!(!locks.held("t1"));
        assert!(locks.held("t2"));
        assert_eq!(locks.tokens(), vec!["t2".to_string()]);
        assert_eq!(locks.count(), 1);
    }

    #[test]
    fn a_lost_lock_stays_lost_after_a_later_ok_verdict() {
        let locks = RunLocks::default();
        locks.add("t1");
        locks.apply_beat(&verdicts(&[("t1", BeatVerdict::Stop)]));
        locks.apply_beat(&verdicts(&[("t1", BeatVerdict::Ok)]));
        assert!(!locks.held("t1"));
    }

    #[test]
    fn clones_share_one_table() {
        let a = RunLocks::default();
        let b = a.clone();
        a.add("t");
        assert!(b.held("t"));
        assert_eq!(format!("{b:?}"), "RunLocks(1 held)");
    }
}
