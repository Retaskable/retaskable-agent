//! Stop and restart with runs live (docs/specs/pieces.md section 3.1 "Stop and restart"): the run loops report how
//! many runs they still hold, and the exit paths wait for that to reach zero, up to [`WAIT_LIMIT`], before cutting
//! what is left. In memory only; nothing is saved to resume.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The most a restart or Quit waits for live runs to end and send their `run_done`.
pub const WAIT_LIMIT: Duration = Duration::from_secs(15 * 60);
/// How often the wait looks again.
const POLL: Duration = Duration::from_millis(100);

static WINDING_DOWN: AtomicBool = AtomicBool::new(false);

fn held() -> &'static Mutex<HashMap<String, usize>> {
    static HELD: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    HELD.get_or_init(Mutex::default)
}

/// What a project's run loop still holds: live runs plus ends not yet accepted. Set after every pass.
pub fn report_held(project: &str, count: usize) {
    held().lock().unwrap().insert(project.into(), count);
}

/// Runs held across every project.
pub fn held_count() -> usize {
    held().lock().unwrap().values().sum()
}

/// Whether the exit paths are waiting for runs: a `runs` loop then takes no new ones.
pub fn winding_down() -> bool {
    WINDING_DOWN.load(Ordering::SeqCst)
}

/// How an exit path waits for runs. The real one reads the process-wide state; a test injects its own.
pub struct RunWait {
    pub limit: Duration,
    pub held: Box<dyn Fn() -> usize + Send + Sync>,
    pub begin: Box<dyn Fn() + Send + Sync>,
}

impl RunWait {
    pub fn real() -> Self {
        Self {
            limit: WAIT_LIMIT,
            held: Box::new(held_count),
            begin: Box::new(|| WINDING_DOWN.store(true, Ordering::SeqCst)),
        }
    }

    /// With runs held, stops taking new ones and blocks until none are held or the limit passes. Returns at once
    /// with none held (the `claim` loop never reports any).
    pub fn wait(&self) {
        if (self.held)() == 0 {
            return;
        }
        (self.begin)();
        let deadline = Instant::now() + self.limit;
        while (self.held)() > 0 && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn wait_with(
        limit: Duration,
        held: impl Fn() -> usize + Send + Sync + 'static,
        begun: &Arc<AtomicBool>,
    ) -> RunWait {
        let begun = begun.clone();
        RunWait {
            limit,
            held: Box::new(held),
            begin: Box::new(move || begun.store(true, Ordering::SeqCst)),
        }
    }

    fn count_of(project: &str) -> usize {
        held().lock().unwrap().get(project).copied().unwrap_or(0)
    }

    #[test]
    fn a_new_report_replaces_that_projects_count_and_others_stay() {
        // Driver tests share this process-wide map, so look only at this test's own projects.
        report_held("wind-test-a", 2);
        report_held("wind-test-b", 3);
        assert_eq!((count_of("wind-test-a"), count_of("wind-test-b")), (2, 3));
        report_held("wind-test-a", 1);
        assert_eq!((count_of("wind-test-a"), count_of("wind-test-b")), (1, 3));
        // The total counts at least both projects.
        assert!(held_count() >= 4);
        report_held("wind-test-a", 0);
        report_held("wind-test-b", 0);
    }

    #[test]
    fn wait_with_nothing_held_returns_without_beginning_to_wind_down() {
        let begun = Arc::new(AtomicBool::new(false));
        wait_with(Duration::from_secs(60), || 0, &begun).wait();
        assert!(!begun.load(Ordering::SeqCst));
    }

    #[test]
    fn wait_begins_then_returns_once_the_runs_are_released() {
        let begun = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let counter = polls.clone();
        // Held for the first three looks, then released.
        let wait = wait_with(
            Duration::from_secs(60),
            move || {
                if counter.fetch_add(1, Ordering::SeqCst) < 3 {
                    1
                } else {
                    0
                }
            },
            &begun,
        );
        let started = Instant::now();
        wait.wait();
        assert!(begun.load(Ordering::SeqCst));
        assert!(polls.load(Ordering::SeqCst) >= 4);
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn wait_gives_up_at_the_limit_with_runs_still_held() {
        let begun = Arc::new(AtomicBool::new(false));
        let wait = wait_with(Duration::from_millis(250), || 1, &begun);
        let started = Instant::now();
        wait.wait();
        let took = started.elapsed();
        assert!(begun.load(Ordering::SeqCst));
        assert!(took >= Duration::from_millis(250), "returned early: {took:?}");
        assert!(took < Duration::from_secs(30), "waited too long: {took:?}");
    }

    #[test]
    fn the_real_wait_uses_the_fifteen_minute_limit() {
        assert_eq!(RunWait::real().limit, Duration::from_secs(15 * 60));
    }
}
