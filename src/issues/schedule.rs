//! When a project's issues are checked (`maybeImport` in `agent/src/issues/import.ts`): at start, every 10 minutes, and
//! at once when the project's `issues` setting changes. The minute's pass of the loop calls [`Issues::tick`] with the
//! settings it read, paused or not (it only adds plan requests, which wait for a person, issue-import.md section 3.2);
//! each check runs as its own task, one at a time per project, and never fails the pass.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::Job;
use super::config::IssuesConfig;
use super::finish::{FINISH_EVERY_MS, finish_issues};
use super::import::{Checked, IMPORT_EVERY_MS, import_issues};

const EVERY: Duration = Duration::from_millis(IMPORT_EVERY_MS);
const FINISH_EVERY: Duration = Duration::from_millis(FINISH_EVERY_MS);

/// One project's checks: when it last checked and with which setting, whether a check is running, and the page's error.
#[derive(Default)]
struct Project {
    last: Option<(Instant, String)>,
    running: Arc<AtomicBool>,
    error: Option<String>,
    /// When the project's merged plans from issues were last looked at, and whether that is running.
    finished_at: Option<Instant>,
    finishing: Arc<AtomicBool>,
}

impl Project {
    /// Whether to check now, and whether the setting changed since the last check (which takes the store's lock at once).
    /// Marks the check as started.
    fn due(&mut self, setting: &str, now: Instant) -> Option<bool> {
        if self.running.load(Ordering::SeqCst) {
            return None;
        }
        let was = self.last.take();
        if let Some((at, key)) = &was
            && key == setting
            && now.duration_since(*at) < EVERY
        {
            self.last = was;
            return None;
        }
        let changed = was.as_ref().is_some_and(|(_, key)| key != setting);
        self.last = Some((now, setting.to_string()));
        Some(changed)
    }
}

impl Project {
    /// Whether to look at the merged plans from issues now (every 10 minutes). Marks the look as started.
    fn finish_due(&mut self, now: Instant) -> bool {
        if self.finishing.load(Ordering::SeqCst)
            || self.finished_at.is_some_and(|at| now.duration_since(at) < FINISH_EVERY)
        {
            return false;
        }
        self.finished_at = Some(now);
        true
    }
}

/// Every project's checks. One per computer ([`Issues::shared`]); a test makes its own.
#[derive(Default)]
pub struct Issues {
    projects: Mutex<HashMap<String, Arc<Mutex<Project>>>>,
}

impl Issues {
    pub fn shared() -> &'static Issues {
        static SHARED: OnceLock<Issues> = OnceLock::new();
        SHARED.get_or_init(Issues::default)
    }

    fn project(&self, id: &str) -> Arc<Mutex<Project>> {
        self.projects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    /// The words the page shows when the project's tracker couldn't be read at the last check (`issueImport.error`); `None`
    /// once a check read it, and when the project has no Import issues setting.
    pub fn import_error(&self, id: &str) -> Option<String> {
        self.project(id).lock().unwrap_or_else(|e| e.into_inner()).error.clone()
    }

    /// Whether a check of the project is running.
    #[cfg(test)]
    pub(crate) fn running(&self, id: &str) -> bool {
        self.project(id)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .running
            .load(Ordering::SeqCst)
    }

    /// Whether a look at the project's merged plans is running.
    #[cfg(test)]
    pub(crate) fn finishing(&self, id: &str) -> bool {
        self.project(id)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finishing
            .load(Ordering::SeqCst)
    }

    /// From the loop: starts a check when the project's setting is on and it's time, or the setting changed, and looks at the
    /// project's merged plans from issues every 10 minutes (to comment on and close the issues whose work is built, which
    /// needs no setting: an issue is finished in its own tracker, whatever the setting is now). `values` is the
    /// project's settings, as the store last gave them. Never fails, never waits.
    pub fn tick(&self, job: Job, values: &Value) {
        let issues = IssuesConfig::of(values);
        let state = self.project(&job.id);
        self.finish(&state, &job, issues.as_ref());
        let Some(issues) = issues else {
            // Without the setting, a page has no error to show.
            state.lock().unwrap_or_else(|e| e.into_inner()).error = None;
            return;
        };
        let (fresh, running) = {
            let mut p = state.lock().unwrap_or_else(|e| e.into_inner());
            // `Value`'s maps are sorted, so the same setting is the same text.
            let setting = values["issues"].to_string();
            let Some(fresh) = p.due(&setting, Instant::now()) else {
                return;
            };
            p.running.store(true, Ordering::SeqCst);
            (fresh, p.running.clone())
        };
        tokio::spawn(async move {
            let done = import_issues(&job, &issues, fresh).await;
            let mut p = state.lock().unwrap_or_else(|e| e.into_inner());
            match done {
                // A row that held an error becomes one without it.
                Checked::Imported(_) => p.error = None,
                Checked::Failed(words) => p.error = Some(words),
                Checked::Skipped => {}
            }
            running.store(false, Ordering::SeqCst);
        });
    }

    fn finish(&self, state: &Arc<Mutex<Project>>, job: &Job, issues: Option<&IssuesConfig>) {
        let finishing = {
            let mut p = state.lock().unwrap_or_else(|e| e.into_inner());
            if !p.finish_due(Instant::now()) {
                return;
            }
            p.finishing.store(true, Ordering::SeqCst);
            p.finishing.clone()
        };
        let (job, issues) = (job.clone(), issues.cloned());
        tokio::spawn(async move {
            let close = issues.as_ref().is_some_and(|i| i.close_when_done);
            finish_issues(&job, close, issues.as_ref()).await;
            finishing.store(false, Ordering::SeqCst);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_at_once_then_every_ten_minutes() {
        let mut p = Project::default();
        let t = Instant::now();
        assert_eq!(p.due("a", t), Some(false), "the first check is at start, not a change");
        assert_eq!(p.due("a", t + Duration::from_secs(599)), None);
        assert_eq!(p.due("a", t + Duration::from_secs(600)), Some(false));
        assert_eq!(
            p.due("a", t + Duration::from_secs(601)),
            None,
            "counted from the last check"
        );
    }

    #[test]
    fn a_changed_setting_is_checked_now_and_says_it_changed() {
        let mut p = Project::default();
        let t = Instant::now();
        p.due("a", t);
        assert_eq!(p.due("b", t + Duration::from_secs(1)), Some(true));
        assert_eq!(p.due("b", t + Duration::from_secs(2)), None);
    }

    #[test]
    fn merged_plans_are_looked_at_every_ten_minutes_and_never_twice_at_once() {
        let mut p = Project::default();
        let t = Instant::now();
        assert!(p.finish_due(t));
        assert!(!p.finish_due(t + Duration::from_secs(599)));
        p.finishing.store(true, Ordering::SeqCst);
        assert!(!p.finish_due(t + Duration::from_secs(700)));
        p.finishing.store(false, Ordering::SeqCst);
        assert!(p.finish_due(t + Duration::from_secs(700)));
    }

    #[test]
    fn nothing_starts_while_a_check_runs() {
        let mut p = Project::default();
        let t = Instant::now();
        p.running.store(true, Ordering::SeqCst);
        assert_eq!(p.due("a", t), None);
        assert!(p.last.is_none(), "a skipped pass doesn't count as a check");
        p.running.store(false, Ordering::SeqCst);
        assert_eq!(p.due("a", t), Some(false));
    }
}
