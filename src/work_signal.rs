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
