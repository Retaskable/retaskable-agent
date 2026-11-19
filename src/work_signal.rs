//! `work_available` (docs/specs/pieces.md section 3.1): the cloud link tells a project's run loop to ask again.
//! The link only wakes it; nothing is run from the message. The store socket (`backlog_socket`) wakes the same
//! loops, and the two things it names besides work: the expert sessions' queue and the deploy requests.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// What a wake is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Wake {
    /// Work may be free: the run loop asks again.
    Work,
    /// The expert sessions' queued messages changed on the site: the sessions read the queue back now.
    ExpertQueue,
    /// A deploy request was added or changed: the deploy loop lists them now.
    DeployRequests,
    /// An intent row was written (`facts_changed` naming `intents`): the loop lists the rows now.
    Intents,
    /// The drafts changed (`drafts_changed`): the plans waiting for approval have their pull requests looked at now.
    Drafts,
    /// Items changed in the store (`changed`): an item parked for its answer is read now, for a person's answer kept on it.
    Changed,
}

/// Each project's wake-ups, shared by whatever sends them (the cloud link, the store socket) and the loops that
/// hear them.
#[derive(Debug, Clone, Default)]
pub struct WorkSignals(
    Arc<Mutex<Signals>>,
    Arc<Mutex<Vec<(String, String, String)>>>,
    Arc<Notify>,
    CiTicks,
);

/// A count per project that goes up with every [`WorkSignals::wake_ci`]; each `watch` run listens to it (a `watch` channel keeps a change that came
/// while the run was reading, and wakes every listener, not one).
#[derive(Debug, Clone)]
struct CiTicks(Arc<tokio::sync::watch::Sender<BTreeMap<String, u64>>>);

impl Default for CiTicks {
    fn default() -> Self {
        Self(Arc::new(tokio::sync::watch::channel(BTreeMap::new()).0))
    }
}

type Signals = BTreeMap<(String, Wake), Arc<Notify>>;

/// Hears the CI wakes of one project.
pub struct CiListener {
    rx: tokio::sync::watch::Receiver<BTreeMap<String, u64>>,
    project: String,
    seen: u64,
}

impl CiListener {
    /// Ends when a CI wake for the project has come since the last call (at once if one did while the caller was busy).
    /// Never ends when the signals are gone.
    pub async fn changed(&mut self) {
        loop {
            let now = self.rx.borrow_and_update().get(&self.project).copied().unwrap_or(0);
            if now != self.seen {
                self.seen = now;
                return;
            }
            if self.rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Wake-ups carry no state a snapshot would compare.
impl PartialEq for WorkSignals {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl WorkSignals {
    /// The project's work signal. A wake sent before the loop listens is kept, so it isn't missed.
    pub fn signal(&self, project: &str) -> Arc<Notify> {
        self.signal_for(project, Wake::Work)
    }

    pub fn wake(&self, project: &str) {
        self.wake_for(project, Wake::Work);
    }

    /// The project's signal for `kind`, kept the same way.
    pub fn signal_for(&self, project: &str, kind: Wake) -> Arc<Notify> {
        let mut all = crate::sync::lock(&self.0);
        all.entry((project.to_string(), kind)).or_default().clone()
    }

    pub fn wake_for(&self, project: &str, kind: Wake) {
        self.signal_for(project, kind).notify_one();
        self.2.notify_one();
    }

    /// Wakes every `watch` run of `project` (a CI result may be in). Unlike the other wakes (one waiter takes it) a project
    /// has several watches, so all hear it; one that was reading when it came hears it at its next wait.
    pub fn wake_ci(&self, project: &str) {
        self.3
            .0
            .send_modify(|ticks| *ticks.entry(project.to_string()).or_default() += 1);
    }

    /// A listener for [`Self::wake_ci`] of `project`: `changed` ends at the next one, or at once when one came since the
    /// listener's last `changed` (or since it was made).
    pub fn ci_listener(&self, project: &str) -> CiListener {
        let mut rx = self.3.0.subscribe();
        let project = project.to_string();
        let seen = rx.borrow_and_update().get(&project).copied().unwrap_or(0);
        CiListener { rx, project, seen }
    }

    /// The one wake the project loop sleeps on (`driver/spawn.rs`): every push it cares about, the end of a run, a queued
    /// Approve and a finished landing task raise it. It only ends the loop's wait early; the pass that follows asks the
    /// store the same questions as a timed one.
    pub fn kick_handle(&self) -> Arc<Notify> {
        self.2.clone()
    }

    /// Ends the project loop's wait now (a pass follows, at most once a second).
    pub fn kick(&self) {
        self.2.notify_one();
    }

    /// A person found a problem with a try-out this computer sent (`tryout_problem`): the project's loop takes it
    /// ([`Self::take_problems`]) and answers the store.
    pub fn problem(&self, project: &str, key: &str, notes: &str) {
        let mut all = crate::sync::lock(&self.1);
        all.push((project.to_string(), key.to_string(), notes.to_string()));
    }

    /// The problems pushed for `project` since the last call: each try-out's key and the person's notes.
    pub fn take_problems(&self, project: &str) -> Vec<(String, String)> {
        let mut all = crate::sync::lock(&self.1);
        let (mine, rest): (Vec<_>, Vec<_>) = all.drain(..).partition(|(p, _, _)| p == project);
        *all = rest;
        mine.into_iter().map(|(_, key, notes)| (key, notes)).collect()
    }

    /// Whether a `kind` wake came since it was last taken; the wake is used up.
    pub fn take(&self, project: &str, kind: Wake) -> bool {
        use futures_util::FutureExt;
        self.signal_for(project, kind).notified().now_or_never().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn woke(n: &Notify) -> bool {
        tokio::time::timeout(Duration::from_millis(50), n.notified())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_wake_sent_before_anyone_listens_is_kept() {
        let signals = WorkSignals::default();
        signals.wake("p");
        assert!(woke(&signals.signal("p")).await);
    }

    #[tokio::test]
    async fn a_wake_reaches_only_its_project() {
        let signals = WorkSignals::default();
        signals.wake("a");
        assert!(!woke(&signals.signal("b")).await);
        assert!(woke(&signals.signal("a")).await);
    }

    #[tokio::test]
    async fn clones_share_signals_and_a_wake_is_consumed_once() {
        let signals = WorkSignals::default();
        let other = signals.clone();
        other.wake("p");
        assert!(Arc::ptr_eq(&signals.signal("p"), &other.signal("p")));
        assert!(woke(&signals.signal("p")).await);
        assert!(!woke(&signals.signal("p")).await);
    }

    #[test]
    fn each_kind_of_wake_is_kept_apart_and_taken_once() {
        let signals = WorkSignals::default();
        signals.wake_for("p", Wake::ExpertQueue);
        assert!(!signals.take("p", Wake::DeployRequests));
        assert!(!signals.take("p", Wake::Work));
        assert!(signals.take("p", Wake::ExpertQueue));
        assert!(!signals.take("p", Wake::ExpertQueue));
    }

    #[test]
    fn every_wake_also_kicks_the_loop_and_clones_share_the_kick() {
        use futures_util::FutureExt;
        let signals = WorkSignals::default();
        let kick = signals.clone().kick_handle();
        assert!(kick.notified().now_or_never().is_none());
        signals.wake_for("p", Wake::Intents);
        assert!(kick.notified().now_or_never().is_some());
        assert!(kick.notified().now_or_never().is_none());
        signals.kick();
        assert!(kick.notified().now_or_never().is_some());
    }

    #[test]
    fn a_problem_goes_to_its_project_once() {
        let signals = WorkSignals::default();
        signals.problem("a", "k1", "it broke");
        signals.problem("b", "k2", "other");
        assert_eq!(
            signals.take_problems("a"),
            vec![("k1".to_string(), "it broke".to_string())]
        );
        assert!(signals.take_problems("a").is_empty());
        assert_eq!(signals.take_problems("b").len(), 1);
    }

    #[test]
    fn signals_always_compare_equal() {
        let a = WorkSignals::default();
        a.wake("p");
        assert_eq!(a, WorkSignals::default());
    }
}
