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
    /// The id of the spec draft being written now, if any (it holds no lane).
    pub drafting: Option<String>,
    /// What the idle baseline last found on the base branch (fix main), reported with the computer's state.
    pub fix_main: Option<String>,
}

/// The machine-wide mode plus one entry per project whose loop runs. Empty when the mode is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveState {
    pub mode: OrchestratorMode,
    /// In `runs` mode, the kinds of run this computer can take (`plan`, `build`, ...); empty with no runner.
    pub run_kinds: Vec<String>,
    pub projects: BTreeMap<String, ProjectLive>,
}

/// Shared between the loops (writers) and the status handler (reader).
#[derive(Debug, Clone, Default)]
pub struct LiveHandle(Arc<Mutex<LiveState>>);

impl LiveHandle {
    pub fn new(mode: OrchestratorMode) -> Self {
        Self(Arc::new(Mutex::new(LiveState {
            mode,
            run_kinds: Vec::new(),
            projects: BTreeMap::new(),
        })))
    }

    /// Says which kinds of run `runs` mode can take.
    pub fn set_run_kinds(&self, kinds: Vec<String>) {
        self.lock().run_kinds = kinds;
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
            drafting: entry.drafting.take(),
            fix_main: entry.fix_main.take(),
        };
    }

    /// Says which draft the project is writing now, or none.
    pub fn set_drafting(&self, project: &str, draft: Option<String>) {
        self.lock().projects.entry(project.to_string()).or_default().drafting = draft;
    }

    /// Says what the idle baseline found, or nothing.
    pub fn set_fix_main(&self, project: &str, note: Option<String>) {
        self.lock().projects.entry(project.to_string()).or_default().fix_main = note;
    }

    /// A pass that was skipped for a store error. The tick fields stay as the last good pass left them.
    pub fn failed(&self, project: &str, at: String, message: String) {
        let mut state = self.lock();
        let entry = state.projects.entry(project.to_string()).or_default();
        let since = entry.store_error.take().map_or(at, |f| f.since);
        entry.store_error = Some(StoreFailure { since, message });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LiveState> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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

    fn held(lane: u32, item: &str) -> Vec<HeldLane> {
        vec![HeldLane {
            lane,
            item_id: item.to_string(),
            title: None,
        }]
    }

    #[test]
    fn register_adds_an_empty_entry_and_keeps_an_existing_one() {
        let live = LiveHandle::new(OrchestratorMode::default());
        live.register("p");
        assert_eq!(live.snapshot().projects["p"], ProjectLive::default());
        live.ticked("p", true, "t1".into(), held(1, "a"), "d".into());
        live.register("p");
        assert_eq!(live.snapshot().projects["p"].last_tick.as_deref(), Some("t1"));
    }

    #[test]
    fn a_tick_replaces_the_entry_and_clears_the_store_error() {
        let live = LiveHandle::default();
        live.failed("p", "t0".into(), "no link".into());
        live.ticked("p", false, "t1".into(), held(2, "item"), "would ask for work".into());
        let p = &live.snapshot().projects["p"];
        assert!(p.store_error.is_none() && !p.paused);
        assert_eq!(p.last_tick.as_deref(), Some("t1"));
        assert_eq!(p.held, held(2, "item"));
        assert_eq!(p.decision.as_deref(), Some("would ask for work"));
    }

    #[test]
    fn a_failure_keeps_the_last_good_tick_and_the_streaks_start() {
        let live = LiveHandle::default();
        live.ticked("p", false, "t1".into(), held(1, "a"), "d".into());
        live.failed("p", "t2".into(), "first".into());
        live.failed("p", "t3".into(), "second".into());
        let p = &live.snapshot().projects["p"];
        let err = p.store_error.as_ref().unwrap();
        assert_eq!(err.since, "t2");
        assert_eq!(err.message, "second");
        assert_eq!(p.last_tick.as_deref(), Some("t1"));
        assert_eq!(p.held, held(1, "a"));
    }

    #[test]
    fn a_new_failure_after_a_tick_starts_a_new_streak() {
        let live = LiveHandle::default();
        live.failed("p", "t1".into(), "x".into());
        live.ticked("p", false, "t2".into(), vec![], "d".into());
        live.failed("p", "t3".into(), "y".into());
        assert_eq!(live.snapshot().projects["p"].store_error.as_ref().unwrap().since, "t3");
    }

    #[test]
    fn clones_share_state_and_now_is_rfc3339() {
        let live = LiveHandle::default();
        live.clone().register("shared");
        assert!(live.snapshot().projects.contains_key("shared"));
        assert!(now_iso8601().ends_with('Z'));
    }
}
