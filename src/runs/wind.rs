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
