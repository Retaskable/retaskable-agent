//! A pending Restart (docs/specs/agent/09-web-ui.md section 9.3): the first request's `why`, the "finish first"
//! registry of work a restart waits for, and the step the snapshot reports. The sequence's other steps (cutting
//! sessions, installing, exiting) are `runtime::cut_every_session_then_exit`'s.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;

/// The most a restart waits for work on the finish-first list.
pub const FINISH_FIRST: Duration = Duration::from_secs(2 * 60);
/// The most a restart waits while a TestFlight build runs, then it goes even if the build hangs.
pub const TESTFLIGHT_FIRST: Duration = Duration::from_secs(45 * 60);
/// The most the cloud link gets to send its same-tick answers before the exit.
pub const FLUSH: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);

/// Who asked for a restart, when and why: the first request's, kept.
#[derive(Debug, Clone, PartialEq)]
pub struct Why {
    pub at: i64,
    pub by: String,
    pub reason: String,
}

/// Where a pending restart is.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Still waiting for these (what is on the finish-first list and held).
    Waiting(Vec<String>),
    Now,
}

const TESTFLIGHT: &str = "a TestFlight build";

#[derive(Default)]
struct Inner {
    why: Option<Why>,
    step: Option<Step>,
    held: Vec<(u64, String)>,
}

/// The process-wide restart state, see [`state`]. Tests make their own.
pub struct RestartState {
    inner: Mutex<Inner>,
    next: AtomicU64,
    started: AtomicBool,
    /// Whether a TestFlight build runs; a test swaps it.
    build_running: fn() -> bool,
    changed: watch::Sender<u64>,
}

impl std::fmt::Debug for RestartState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RestartState({:?})", self.report())
    }
}

impl PartialEq for RestartState {
    fn eq(&self, other: &Self) -> bool {
        self.report() == other.report()
    }
}

impl Default for RestartState {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            next: AtomicU64::new(0),
            started: AtomicBool::new(false),
            build_running: crate::testflight::testflight_running,
            changed: watch::Sender::new(0),
        }
    }
}

/// Held while a merge, a plan's merge, an update to main or a promote runs; a restart waits until it is dropped.
pub struct Guard {
    state: Arc<RestartState>,
    id: u64,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.state.inner.lock().unwrap().held.retain(|(id, _)| *id != self.id);
    }
}

/// The restart state of this process.
pub fn state() -> &'static Arc<RestartState> {
    static STATE: OnceLock<Arc<RestartState>> = OnceLock::new();
    STATE.get_or_init(Arc::default)
}

impl RestartState {
    /// Records a restart request. Returns whether it is the first: asking again keeps the first `why`.
    pub fn ask(&self, why: Why) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.why.is_some() {
            return false;
        }
        inner.why = Some(why);
        inner.step = Some(Step::Waiting(Vec::new()));
        drop(inner);
        self.notify();
        true
    }

    /// Whether this call is the one to run the sequence: true once, so a second request starts nothing more.
    pub fn start(&self) -> bool {
        !self.started.swap(true, Ordering::SeqCst)
    }

    pub fn why(&self) -> Option<Why> {
        self.inner.lock().unwrap().why.clone()
    }

    /// `None` while no restart is pending.
    pub fn step(&self) -> Option<Step> {
        self.inner.lock().unwrap().step.clone()
    }

    /// Marks `what` as work a restart waits for, until the guard drops.
    pub fn hold(self: &Arc<Self>, what: &str) -> Guard {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.inner.lock().unwrap().held.push((id, what.to_string()));
        Guard {
            state: self.clone(),
            id,
        }
    }

    /// What a restart or quit waits for: what is held, then "a TestFlight build" while one runs.
    pub fn held(&self) -> Vec<String> {
        let mut list: Vec<String> = self.inner.lock().unwrap().held.iter().map(|(_, w)| w.clone()).collect();
        if (self.build_running)() {
            list.push(TESTFLIGHT.to_string());
        }
        list
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// The pending restart for a snapshot (`{ step, waitingOn?, at, by, reason }`), `None` when there isn't one.
    pub fn report(&self) -> Option<Value> {
        let inner = self.inner.lock().unwrap();
        let (why, step) = (inner.why.as_ref()?, inner.step.as_ref()?);
        let mut out = json!({ "at": why.at, "by": why.by, "reason": why.reason });
        match step {
            Step::Waiting(on) => {
                out["step"] = json!("waiting");
                out["waitingOn"] = json!(on);
            }
            Step::Now => out["step"] = json!("now"),
        }
        Some(out)
    }

    fn set_step(&self, step: Step) {
        self.inner.lock().unwrap().step = Some(step);
        self.notify();
    }

    fn notify(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// Step 2: waits while anything is held, [`FINISH_FIRST`] at most ([`TESTFLIGHT_FIRST`] while a build runs) by `clock`, reporting `waiting` with what it
    /// waits on. `on_waiting` runs once, when the first wait starts, with what it waits on and the ms waited so far
    /// at its end (for `restart_waiting`). Ends with the step `now`.
    pub fn finish_first(&self, clock: &Clock, on_waiting: &mut dyn FnMut(Vec<String>, u64)) {
        let started = (clock.now_ms)();
        let first = self.held();
        if !first.is_empty() {
            self.set_step(Step::Waiting(first.clone()));
            let mut last = first.clone();
            loop {
                let now_held = self.held();
                let cap = if now_held.iter().any(|w| w == TESTFLIGHT) {
                    TESTFLIGHT_FIRST
                } else {
                    FINISH_FIRST
                };
                if (clock.now_ms)() - started >= cap.as_millis() as i64 {
                    break;
                }
                if now_held.is_empty() {
                    break;
                }
                if now_held != last {
                    self.set_step(Step::Waiting(now_held.clone()));
                    last = now_held;
                }
                (clock.sleep)(POLL);
            }
            on_waiting(first, ((clock.now_ms)() - started).max(0) as u64);
        }
        self.set_step(Step::Now);
    }
}

/// Time, injected so a test can pass two minutes at once.
pub struct Clock {
    pub now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    pub sleep: Box<dyn Fn(Duration) + Send + Sync>,
}

impl Clock {
    pub fn real() -> Self {
        Self {
            now_ms: Box::new(crate::telemetry::now_ms),
            sleep: Box::new(std::thread::sleep),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::atomic::AtomicI64;

    use super::*;

    fn why(by: &str) -> Why {
        Why {
            at: 1,
            by: by.into(),
            reason: format!("by {by}"),
        }
    }

    /// A clock that moves only when slept, running `each` at every sleep.
    fn fake(each: impl Fn(i64) + Send + Sync + 'static) -> Clock {
        let t = Arc::new(AtomicI64::new(0));
        let t2 = t.clone();
        Clock {
            now_ms: Box::new(move || t.load(Ordering::SeqCst)),
            sleep: Box::new(move |d| {
                let now = t2.fetch_add(d.as_millis() as i64, Ordering::SeqCst) + d.as_millis() as i64;
                each(now);
            }),
        }
    }

    #[test]
    fn a_second_request_keeps_the_first_why() {
        let s = RestartState::default();
        assert!(s.ask(why("ann")));
        assert!(!s.ask(why("bob")));
        assert_eq!(s.why().unwrap().by, "ann");
        assert_eq!(s.report().unwrap()["reason"], "by ann");
    }

    #[test]
    fn nothing_held_goes_to_now_with_no_waiting() {
        let s = RestartState::default();
        s.ask(why("ann"));
        let waited = Cell::new(false);
        s.finish_first(&fake(|_| {}), &mut |_, _| waited.set(true));
        assert!(!waited.get());
        assert_eq!(s.step(), Some(Step::Now));
    }

    #[test]
    fn a_held_guard_reports_waiting_then_now_once_released() {
        let s = Arc::new(RestartState::default());
        s.ask(why("ann"));
        let guard = Mutex::new(Some(s.hold("a merge")));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (s2, seen2) = (s.clone(), seen.clone());
        let clock = fake(move |now| {
            seen2.lock().unwrap().push(s2.report().unwrap()["step"].clone());
            if now >= 300 {
                guard.lock().unwrap().take();
            }
        });
        let mut waiting = None;
        s.finish_first(&clock, &mut |on, ms| waiting = Some((on, ms)));
        assert_eq!(waiting, Some((vec!["a merge".to_string()], 300)));
        assert!(seen.lock().unwrap().iter().all(|v| v == "waiting"));
        assert_eq!(s.step(), Some(Step::Now));
    }

    #[test]
    fn it_goes_anyway_after_two_minutes() {
        let s = Arc::new(RestartState::default());
        s.ask(why("ann"));
        let _held = s.hold("a promote");
        let mut ms = 0;
        s.finish_first(&fake(|_| {}), &mut |_, m| ms = m);
        assert_eq!(ms, FINISH_FIRST.as_millis() as u64);
        assert_eq!(s.step(), Some(Step::Now));
    }

    #[test]
    fn a_running_build_is_waited_on_for_45_minutes() {
        let s = RestartState {
            build_running: || true,
            ..Default::default()
        };
        s.ask(why("ann"));
        assert_eq!(s.held(), vec![TESTFLIGHT.to_string()]);
        let mut seen = None;
        s.finish_first(&fake(|_| {}), &mut |on, m| seen = Some((on, m)));
        assert_eq!(
            seen,
            Some((vec![TESTFLIGHT.to_string()], TESTFLIGHT_FIRST.as_millis() as u64))
        );
        assert_eq!(s.step(), Some(Step::Now));
    }
}
