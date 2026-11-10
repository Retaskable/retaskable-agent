//! The base checkout's busy lock ([`crate::lanes::base_checkout_path`]): Shoot, Play main, the baseline and
//! the minute sync each hold it while they use the checkout, so each is refused (or, for the sync, put off
//! and run on the next poll after the hold is given back) while another has it.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use crate::sync::lock;

#[derive(Default)]
struct State {
    held: HashMap<String, &'static str>,
    /// Projects whose minute sync was put off because the base was held.
    wanted: HashSet<String>,
    /// Put-off syncs whose hold has since been given back; read and cleared by [`take_due_syncs`].
    due: HashSet<String>,
}

static BASE_HELD: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

/// A project's hold on its base checkout ([`hold_base`]); released on drop.
pub struct BaseHold(String);

impl Drop for BaseHold {
    fn drop(&mut self) {
        let mut state = lock(&BASE_HELD);
        state.held.remove(&self.0);
        if state.wanted.remove(&self.0) {
            state.due.insert(self.0.clone());
        }
    }
}

/// Takes `project`'s base checkout for `holder` ("a shoot", "Play main"), or says who has it.
pub fn hold_base(project: &str, holder: &'static str) -> Result<BaseHold, String> {
    let mut state = lock(&BASE_HELD);
    if let Some(who) = state.held.get(project) {
        return Err(format!("The base checkout is busy: {who} is using it."));
    }
    state.held.insert(project.to_string(), holder);
    Ok(BaseHold(project.to_string()))
}

/// Takes the base checkout for a sync ("a sync"). When it's held, the sync is put off instead: `project` is
/// marked wanted (in the same step as the check, so a release can't slip between) and `None` comes back.
pub fn hold_base_for_sync(project: &str) -> Option<BaseHold> {
    let mut state = lock(&BASE_HELD);
    if state.held.contains_key(project) {
        state.wanted.insert(project.to_string());
        return None;
    }
    state.held.insert(project.to_string(), "a sync");
    Some(BaseHold(project.to_string()))
}

/// The projects (ids without `scope`, which is empty in the agent) whose put-off sync is now due (the hold
/// was given back), each once.
pub fn take_due_syncs(scope: &str) -> HashSet<String> {
    let mut state = lock(&BASE_HELD);
    let (mine, rest) = std::mem::take(&mut state.due)
        .into_iter()
        .partition(|k: &String| k.starts_with(scope));
    state.due = rest;
    mine.into_iter().map(|k: String| k[scope.len()..].to_string()).collect()
}

/// Who holds `project`'s base checkout, if anyone.
pub fn base_holder(project: &str) -> Option<&'static str> {
    lock(&BASE_HELD).held.get(project).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_base_refuses_a_second_holder_and_names_the_first() {
        let first = hold_base("base-hold-t1", "a shoot").unwrap();
        assert_eq!(base_holder("base-hold-t1"), Some("a shoot"));
        let err = hold_base("base-hold-t1", "Play main").err().unwrap();
        assert_eq!(err, "The base checkout is busy: a shoot is using it.");
        drop(first);
    }

    #[test]
    fn dropping_the_hold_frees_the_base() {
        drop(hold_base("base-hold-t2", "Play main").unwrap());
        assert_eq!(base_holder("base-hold-t2"), None);
        assert!(hold_base("base-hold-t2", "a shoot").is_ok());
    }

    #[test]
    fn projects_hold_their_bases_independently() {
        let _a = hold_base("base-hold-t3a", "a shoot").unwrap();
        assert!(hold_base("base-hold-t3b", "Play main").is_ok());
    }

    #[test]
    fn a_sync_blocks_the_others_and_each_blocks_a_sync() {
        let sync = hold_base_for_sync("base-hold-t4").unwrap();
        for who in ["a shoot", "Play main", "the baseline"] {
            let err = hold_base("base-hold-t4", who).err().unwrap();
            assert_eq!(err, "The base checkout is busy: a sync is using it.");
        }
        drop(sync);
        for who in ["a shoot", "Play main", "the baseline"] {
            let hold = hold_base("base-hold-t4", who).unwrap();
            assert!(hold_base_for_sync("base-hold-t4").is_none());
            drop(hold);
        }
    }

    #[test]
    fn a_put_off_sync_is_due_once_when_the_holder_drops() {
        let shoot = hold_base("bh5:p", "a shoot").unwrap();
        assert!(hold_base_for_sync("bh5:p").is_none());
        assert!(!take_due_syncs("bh5:").contains("p"));
        drop(shoot);
        assert!(take_due_syncs("bh5:").contains("p"));
        assert!(!take_due_syncs("bh5:").contains("p"));
        drop(hold_base("bh5:p", "a shoot").unwrap());
        assert!(!take_due_syncs("bh5:").contains("p"));
    }

    #[test]
    fn a_put_off_sync_is_independent_of_other_projects() {
        let _a = hold_base("bh6:a", "a shoot").unwrap();
        assert!(hold_base_for_sync("bh6:a").is_none());
        let b = hold_base_for_sync("bh6:b").unwrap();
        drop(b);
        assert!(!take_due_syncs("bh6:").contains("b"));
    }
}
