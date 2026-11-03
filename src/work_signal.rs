//! `work_available` (docs/specs/pieces.md section 3.1): the cloud link tells a project's run loop to ask again.
//! The link only wakes it; nothing is run from the message.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// Each project's wake-up, shared by the cloud link (which sends it) and the project's run loop (which hears it).
#[derive(Debug, Clone, Default)]
pub struct WorkSignals(Arc<Mutex<BTreeMap<String, Arc<Notify>>>>);

/// Wake-ups carry no state a snapshot would compare.
impl PartialEq for WorkSignals {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl WorkSignals {
    /// The project's signal. A wake sent before the loop listens is kept, so it isn't missed.
    pub fn signal(&self, project: &str) -> Arc<Notify> {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        all.entry(project.to_string()).or_default().clone()
    }

    pub fn wake(&self, project: &str) {
        self.signal(project).notify_one();
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
    fn signals_always_compare_equal() {
        let a = WorkSignals::default();
        a.wake("p");
        assert_eq!(a, WorkSignals::default());
    }
}
