//! Whether a TestFlight build is running on this computer. A restart (and quit) waits for one
//! (docs/specs/testflight-restart-wait.md). The Rust agent has no build of its own yet: whatever runs one holds a
//! [`Running`] for as long as it does, and until then this is false.
use std::sync::atomic::{AtomicUsize, Ordering};

static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// True while any TestFlight build runs.
pub fn testflight_running() -> bool {
    RUNNING.load(Ordering::SeqCst) > 0
}

/// A running build: held from its start until it drops (on error or panic too).
pub struct Running(());

impl Running {
    #[allow(dead_code)]
    pub fn begin() -> Self {
        RUNNING.fetch_add(1, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}
