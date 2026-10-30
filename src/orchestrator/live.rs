//! What the orchestrator loop is doing right now, kept small for the settings page's Now tab
//! (`GET /api/settings/status`). Each project's loop writes its entry after every pass; the status
//! handler takes a snapshot. Names and times only, nothing the loop needs to read back.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::driver::OrchestratorMode;

/// A lane a project's loop holds, and what holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldLane {
    pub lane: u32,
    pub item_id: String,
    /// The item's title, when the last read of the backlog had it.
    pub title: Option<String>,
}

/// The backlog could not be reached (or answered something unreadable) on the last pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreFailure {
    /// When the failing streak began (ISO 8601); a later failure keeps it.
    pub since: String,
    pub message: String,
}

/// One project's loop, as of its last pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectLive {
    pub paused: bool,
    /// When the last pass finished with a tick (ISO 8601); `None` before the first.
    pub last_tick: Option<String>,
    pub held: Vec<HeldLane>,
    /// What the last tick would have done: "would ask for work", "would start <item> on lane N".
    pub decision: Option<String>,
    /// Set while the last attempt failed; cleared by the next tick.
    pub store_error: Option<StoreFailure>,
}

/// The machine-wide mode plus one entry per project whose loop runs. Empty when the mode is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveState {
    pub mode: OrchestratorMode,
    pub projects: BTreeMap<String, ProjectLive>,
}

/// Shared between the loops (writers) and the status handler (reader).
#[derive(Debug, Clone, Default)]
pub struct LiveHandle(Arc<Mutex<LiveState>>);

impl LiveHandle {
    pub fn new(mode: OrchestratorMode) -> Self {
        Self(Arc::new(Mutex::new(LiveState {
            mode,
            projects: BTreeMap::new(),
        })))
    }

    pub fn snapshot(&self) -> LiveState {
        self.lock().clone()
    }

    /// Registers a project's loop, so the page knows it runs before its first pass ends.
    pub fn register(&self, project: &str) {
        self.lock().projects.entry(project.to_string()).or_default();
    }

    /// A pass that ticked: replaces the entry and clears any store error.
    pub fn ticked(&self, project: &str, paused: bool, at: String, held: Vec<HeldLane>, decision: String) {
        let mut state = self.lock();
        let entry = state.projects.entry(project.to_string()).or_default();
        *entry = ProjectLive {
            paused,
            last_tick: Some(at),
            held,
            decision: Some(decision),
            store_error: None,
        };
    }

    /// A pass that was skipped for a store error. The tick fields stay as the last good pass left them.
    pub fn failed(&self, project: &str, at: String, message: String) {
        let mut state = self.lock();
        let entry = state.projects.entry(project.to_string()).or_default();
        let since = entry.store_error.take().map_or(at, |f| f.since);
        entry.store_error = Some(StoreFailure { since, message });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LiveState> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Now, as ISO 8601 (UTC).
pub fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_adds_a_default_entry_but_never_overwrites_an_existing_one() {
        let live = LiveHandle::new(OrchestratorMode::Dry);
        live.ticked("p0", true, "t1".to_string(), vec![], "would ask for work".to_string());

        live.register("p0");
        live.register("p1");

        let snap = live.snapshot();
        assert_eq!(snap.projects["p0"].last_tick, Some("t1".to_string()));
        assert_eq!(snap.projects["p1"], ProjectLive::default());
    }

    #[test]
    fn ticked_replaces_the_entry_and_clears_any_store_error() {
        let live = LiveHandle::new(OrchestratorMode::Claim);
        live.failed("p0", "t1".to_string(), "no link".to_string());

        let held = vec![HeldLane {
            lane: 0,
            item_id: "I-1".to_string(),
            title: Some("Fix it".to_string()),
        }];
        live.ticked(
            "p0",
            false,
            "t2".to_string(),
            held.clone(),
            "would start I-1 on lane 0".to_string(),
        );

        let entry = live.snapshot().projects["p0"].clone();
        assert_eq!(
            entry,
            ProjectLive {
                paused: false,
                last_tick: Some("t2".to_string()),
                held,
                decision: Some("would start I-1 on lane 0".to_string()),
                store_error: None,
            }
        );
    }

    #[test]
    fn failed_keeps_the_first_failures_since_across_a_run_of_failures() {
        let live = LiveHandle::new(OrchestratorMode::Dry);

        live.failed("p0", "t1".to_string(), "no link".to_string());
        live.failed("p0", "t2".to_string(), "still no link".to_string());

        let error = live.snapshot().projects["p0"].store_error.clone().unwrap();
        assert_eq!(error.since, "t1");
        assert_eq!(error.message, "still no link");
    }

    #[test]
    fn failed_after_a_tick_starts_a_fresh_since() {
        let live = LiveHandle::new(OrchestratorMode::Dry);
        live.failed("p0", "t1".to_string(), "no link".to_string());
        live.ticked("p0", false, "t2".to_string(), vec![], "would ask for work".to_string());

        live.failed("p0", "t3".to_string(), "no link again".to_string());

        let error = live.snapshot().projects["p0"].store_error.clone().unwrap();
        assert_eq!(error.since, "t3");
    }

    #[test]
    fn snapshot_carries_the_machine_wide_mode() {
        let live = LiveHandle::new(OrchestratorMode::Claim);
        assert_eq!(live.snapshot().mode, OrchestratorMode::Claim);
    }
}
