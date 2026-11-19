//! The open `retaskable/` pull requests this computer has seen, for `health`'s `openChanges` (Retaskable.com
//! pieces/15, 2026-11-19): the CI pass already lists a project's open pull requests, so it keeps the agent's own here
//! (number, branch, head; at most 50) and `health` sends them at most every 10 minutes. The store maps each branch to its
//! item and piece and closes the ones nobody owns; the agent decides nothing.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::ci_pulls::OpenPull;

/// The most pull requests reported, and how often `health` carries them.
pub const MAX: usize = 50;
pub const EVERY: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenChange {
    pub pr: u64,
    pub branch: String,
    pub head: String,
}

struct Seen {
    list: Vec<OpenChange>,
    /// When `health` last carried this list.
    sent: Option<Instant>,
    fresh: bool,
}

fn table() -> &'static Mutex<HashMap<String, Seen>> {
    static T: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

/// The agent's own pull requests among `pulls` (a `retaskable/` branch), at most [`MAX`].
pub fn own(pulls: &[OpenPull]) -> Vec<OpenChange> {
    pulls
        .iter()
        .filter(|p| p.branch.starts_with("retaskable/"))
        .take(MAX)
        .map(|p| OpenChange {
            pr: p.number,
            branch: p.branch.clone(),
            head: p.head.clone(),
        })
        .collect()
}

/// Keeps what the pass listed for `repo`.
pub fn record(repo: &str, pulls: &[OpenPull]) {
    let list = own(pulls);
    let mut t = table().lock().unwrap();
    let e = t.entry(repo.to_string()).or_insert(Seen {
        list: Vec::new(),
        sent: None,
        fresh: false,
    });
    e.list = list;
    e.fresh = true;
}

/// What `health` carries now for `repo`: the list when it is new and [`EVERY`] has passed since it last went (an empty list
/// is sent too, so the store drops what closed); `None` otherwise.
pub fn due(repo: &str, now: Instant) -> Option<Vec<OpenChange>> {
    let mut t = table().lock().unwrap();
    let e = t.get_mut(repo)?;
    if !e.fresh || e.sent.is_some_and(|at| now.duration_since(at) < EVERY) {
        return None;
    }
    e.fresh = false;
    e.sent = Some(now);
    Some(e.list.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(n: u64, branch: &str) -> OpenPull {
        OpenPull {
            number: n,
            title: String::new(),
            url: String::new(),
            branch: branch.into(),
            head: "abc".into(),
            author: None,
            mergeable: None,
            cross_repository: false,
        }
    }

    #[test]
    fn keeps_only_agent_branches_at_most_fifty() {
        let mut pulls: Vec<OpenPull> = (0..80).map(|n| pull(n, "retaskable/x-piece-1-a-s0")).collect();
        pulls.push(pull(999, "feature/x"));
        let got = own(&pulls);
        assert_eq!(got.len(), MAX);
        assert!(got.iter().all(|c| c.branch.starts_with("retaskable/")));
    }

    #[test]
    fn health_carries_a_new_list_at_most_every_ten_minutes() {
        let repo = "o/open-changes-test";
        let t0 = Instant::now();
        assert_eq!(due(repo, t0), None, "nothing seen yet");
        record(repo, &[pull(7, "retaskable/a-piece-1-x-s0")]);
        assert_eq!(due(repo, t0).unwrap().len(), 1);
        assert_eq!(due(repo, t0), None, "sent already");
        record(repo, &[]);
        assert_eq!(due(repo, t0 + Duration::from_secs(60)), None, "too soon");
        assert_eq!(due(repo, t0 + EVERY).unwrap(), Vec::new(), "an empty list goes too");
    }
}
