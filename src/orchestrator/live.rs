//! What the orchestrator loop is doing right now, kept small for the settings page's Now tab
//! (`GET /api/settings/status`). Each project's loop writes its entry after every pass; the status
//! handler takes a snapshot. Names and times only, nothing the loop needs to read back.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::driver::OrchestratorMode;
use crate::telemetry::debounce::OpenAnomaly;

pub mod sessions;

/// A lane a project's loop holds, and what holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldLane {
    pub lane: u32,
    pub item_id: String,
    /// The item's title, when the last read of the backlog had it.
    pub title: Option<String>,
    /// The branch the item's slice is on, when it is known.
    pub branch: Option<String>,
    /// Whether a game runs in the lane (Play).
    pub playing: bool,
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
    /// Why the last main sync didn't start, when it didn't.
    pub sync_main: Option<String>,
    /// The store's last answer of rotation chores started today and the day's limit; `None` before one.
    pub chores: Option<crate::backlog::runs::ChoreCount>,
    /// The last vet run's counts by verdict name; `None` before a run.
    pub vet: Option<BTreeMap<String, usize>>,
    /// When Claude's limit lifts (ISO 8601), while the loop can't start work because Claude is at its limit.
    pub limit_resets_at: Option<String>,
    /// The instant `limit_resets_at` was made from, so the same limit keeps the same string pass after pass.
    pub limit_until: Option<std::time::Instant>,
    /// The watchdog's open anomalies as of its last minute, oldest first.
    pub anomalies: Vec<crate::telemetry::debounce::OpenAnomaly>,
}

/// The machine-wide mode plus one entry per project whose loop runs. Empty when the mode is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveState {
    pub mode: OrchestratorMode,
    /// In `runs` mode, the kinds of run this computer can take (`plan`, `build`, ...); empty with no runner.
    pub run_kinds: Vec<String>,
    pub projects: BTreeMap<String, ProjectLive>,
    /// Agent CI's slot, Docker and discovery (agent-ci.md); `None` unless the `agent-ci` loop runs.
    pub ci: Option<crate::agent_ci::CiLive>,
    /// How many sessions the shared pool allows at once (auto concurrency's number once it has stepped); 0 before a pass.
    pub concurrency: u32,
}

/// What the snapshot reports of the loops: a change in it is what [`LiveHandle::subscribe`] signals.
type Reported = (
    u32,
    BTreeMap<String, (bool, Option<String>, Vec<OpenAnomaly>, Vec<HeldLane>)>,
);

fn reported(state: &LiveState) -> Reported {
    let projects = state
        .projects
        .iter()
        .map(|(id, p)| {
            (
                id.clone(),
                (p.paused, p.limit_resets_at.clone(), p.anomalies.clone(), p.held.clone()),
            )
        })
        .collect();
    (state.concurrency, projects)
}

/// Shared between the loops (writers) and the status handler (reader).
#[derive(Debug, Clone, Default)]
pub struct LiveHandle(Arc<Mutex<LiveState>>, sessions::Sessions, Arc<watch::Sender<u64>>);

impl PartialEq for LiveHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl LiveHandle {
    /// Sets the machine-wide mode, once it is known.
    pub fn with_mode(self, mode: OrchestratorMode) -> Self {
        self.lock().mode = mode;
        self
    }

    pub fn new(mode: OrchestratorMode) -> Self {
        Self(
            Arc::new(Mutex::new(LiveState {
                mode,
                run_kinds: Vec::new(),
                projects: BTreeMap::new(),
                ci: None,
                concurrency: 0,
            })),
            sessions::Sessions::default(),
            Arc::default(),
        )
    }

    /// The sessions running now; the loop registers them and the status handler reads them.
    pub fn sessions(&self) -> &sessions::Sessions {
        &self.1
    }

    /// Has session ends add Cursor's and Codex's tokens to machine.json in `dir`.
    pub fn with_machine_dir(self, dir: std::path::PathBuf) -> Self {
        Self(self.0, self.1.with_machine_dir(dir), self.2)
    }

    /// Says which kinds of run `runs` mode can take.
    pub fn set_run_kinds(&self, kinds: Vec<String>) {
        self.lock().run_kinds = kinds;
    }

    /// Changes whenever a project's reported fields (paused, the limit's reset, a loop appearing) or `concurrency`
    /// change; a pass that leaves them as they were doesn't.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.2.subscribe()
    }

    /// Runs `f` on the state and signals [`subscribe`](Self::subscribe) when what the snapshot reports changed.
    fn edit(&self, f: impl FnOnce(&mut LiveState)) {
        let mut state = self.lock();
        let before = reported(&state);
        f(&mut state);
        if reported(&state) != before {
            self.2.send_modify(|n| *n += 1);
        }
    }

    /// Signals [`subscribe`](Self::subscribe) so the snapshot is resent (Play main started or stopped).
    pub fn bump(&self) {
        self.2.send_modify(|n| *n += 1);
    }

    /// The shared pool's effective concurrency, as of the last pass.
    pub fn set_concurrency(&self, concurrency: u32) {
        self.edit(|s| s.concurrency = concurrency);
    }

    /// Says when Claude's limit lifts while the project can't start work for it, or nothing.
    pub fn set_limit(&self, project: &str, until: Option<std::time::Instant>) {
        self.edit(|s| {
            let entry = s.projects.entry(project.to_string()).or_default();
            if entry.limit_until != until {
                entry.limit_until = until;
                entry.limit_resets_at =
                    until.map(|u| iso8601_in(u.saturating_duration_since(std::time::Instant::now())));
            }
        });
    }

    /// The watchdog's open anomalies for the project, as of this minute's check.
    pub fn set_anomalies(&self, project: &str, anomalies: Vec<OpenAnomaly>) {
        self.edit(|s| s.projects.entry(project.to_string()).or_default().anomalies = anomalies);
    }

    pub fn snapshot(&self) -> LiveState {
        self.lock().clone()
    }

    /// The CI loop's poll writes its fields.
    pub fn update_ci(&self, f: impl FnOnce(&mut crate::agent_ci::CiLive)) {
        f(self.lock().ci.get_or_insert_default());
    }

    /// Registers a project's loop, so the page knows it runs before its first pass ends.
    pub fn register(&self, project: &str) {
        self.edit(|s| {
            s.projects.entry(project.to_string()).or_default();
        });
    }

    /// A pass that ticked: replaces the entry and clears any store error.
    pub fn ticked(&self, project: &str, paused: bool, at: String, held: Vec<HeldLane>, decision: String) {
        self.edit(|state| {
            let entry = state.projects.entry(project.to_string()).or_default();
            *entry = ProjectLive {
                paused,
                last_tick: Some(at),
                held,
                decision: Some(decision),
                store_error: None,
                drafting: entry.drafting.take(),
                fix_main: entry.fix_main.take(),
                sync_main: entry.sync_main.take(),
                chores: entry.chores,
                vet: entry.vet.take(),
                limit_resets_at: entry.limit_resets_at.take(),
                limit_until: entry.limit_until.take(),
                anomalies: std::mem::take(&mut entry.anomalies),
            };
        });
    }

    /// Says which draft the project is writing now, or none.
    pub fn set_drafting(&self, project: &str, draft: Option<String>) {
        self.lock().projects.entry(project.to_string()).or_default().drafting = draft;
    }

    /// Says what the idle baseline found, or nothing.
    pub fn set_fix_main(&self, project: &str, note: Option<String>) {
        self.lock().projects.entry(project.to_string()).or_default().fix_main = note;
    }

    /// Says why the main sync didn't start, or nothing.
    pub fn set_sync_main(&self, project: &str, note: Option<String>) {
        self.lock().projects.entry(project.to_string()).or_default().sync_main = note;
    }

    /// Says what the last vet run found, or nothing.
    pub fn set_vet(&self, project: &str, counts: Option<BTreeMap<String, usize>>) {
        self.lock().projects.entry(project.to_string()).or_default().vet = counts;
    }

    /// The store's last chore count for the project.
    pub fn set_chores(&self, project: &str, chores: crate::backlog::runs::ChoreCount) {
        self.lock().projects.entry(project.to_string()).or_default().chores = Some(chores);
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
    iso8601_in(std::time::Duration::ZERO)
}

/// `after` from now, as ISO 8601 (UTC).
pub fn iso8601_in(after: std::time::Duration) -> String {
    (time::OffsetDateTime::now_utc() + after)
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
            branch: None,
            playing: false,
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
    fn subscribers_hear_only_of_changes_to_what_the_snapshot_reports() {
        let live = LiveHandle::default();
        let mut rx = live.subscribe();
        let changed = |rx: &mut watch::Receiver<u64>| {
            rx.has_changed().unwrap() && {
                rx.borrow_and_update();
                true
            }
        };
        live.register("p");
        assert!(changed(&mut rx), "a loop appearing");
        live.register("p");
        live.ticked("p", false, "t1".into(), vec![], "d".into());
        live.ticked("p", false, "t2".into(), vec![], "e".into());
        assert!(!changed(&mut rx), "ticks that change nothing reported");
        live.ticked("p", false, "t2".into(), held(1, "a"), "e".into());
        assert!(changed(&mut rx), "a held lane");
        live.ticked("p", false, "t2".into(), held(1, "a"), "e".into());
        assert!(!changed(&mut rx), "the same lanes again");
        live.ticked("p", true, "t3".into(), vec![], "d".into());
        assert!(changed(&mut rx), "paused");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        live.set_limit("p", Some(until));
        assert!(changed(&mut rx), "limit");
        live.ticked("p", true, "t4".into(), vec![], "d".into());
        assert!(live.snapshot().projects["p"].limit_resets_at.is_some());
        std::thread::sleep(std::time::Duration::from_millis(5));
        live.set_limit("p", Some(until));
        live.set_concurrency(0);
        assert!(!changed(&mut rx), "same values");
        live.set_concurrency(2);
        assert!(changed(&mut rx), "concurrency");
    }

    #[test]
    fn clones_share_state_and_now_is_rfc3339() {
        let live = LiveHandle::default();
        live.clone().register("shared");
        assert!(live.snapshot().projects.contains_key("shared"));
        assert!(now_iso8601().ends_with('Z'));
    }
}
