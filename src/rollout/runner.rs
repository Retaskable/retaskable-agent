//! Which computer the store names as a project's rollout runner (`rolloutRun.runner` in the TS agent): what the
//! CI watcher asks, so one computer per project watches its pull requests (docs/specs/ci-watch-through-store.md
//! section 3, item 2). The rollout loop writes each project's flag from the store's poll answers; the flag is false
//! until a poll has said so, and for a project with no store rollout it never is true.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// Each project's flag; clones share them.
#[derive(Debug, Clone, Default)]
pub struct Runners(
    Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    Arc<Mutex<HashMap<String, Arc<Notify>>>>,
);

impl Runners {
    /// The project's flag, made (false) the first time it is asked for.
    pub fn flag(&self, project: &str) -> Arc<AtomicBool> {
        self.0.lock().unwrap().entry(project.to_string()).or_default().clone()
    }

    /// What wakes the project's rollout loop (made the first time it is asked for): a fetch that moved main, so the new report
    /// goes out at once and a long wait between polls doesn't hold it up.
    pub fn wake(&self, project: &str) -> Arc<Notify> {
        self.1.lock().unwrap().entry(project.to_string()).or_default().clone()
    }

    /// Whether the store's last answer made this computer the project's runner.
    pub fn is_runner(&self, project: &str) -> bool {
        self.flag(project).load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_wake_for_a_project_is_kept_until_its_loop_looks() {
        let runners = Runners::default();
        runners.wake("p").notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), runners.wake("p").notified())
            .await
            .expect("the wake was kept");
        let other = tokio::time::timeout(std::time::Duration::from_millis(50), runners.wake("q").notified()).await;
        assert!(other.is_err(), "another project's wake is its own");
    }
}
