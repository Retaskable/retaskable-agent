//! TestFlight: the build of Retaskable's own iOS app and its upload (`agent/src/testflight.ts`,
//! docs/specs/agent/deploy-requests.md section 2.4). The request itself is `deploy_requests::testflight`; this is the
//! work: the checks in order ([`checks`]), the throwaway checkout and the fixed commands ([`build`]), and what a build
//! asks of the computer ([`tools`], a fake in the tests since the real thing needs a Mac with Xcode and Apple signing).
//!
//! A restart (and quit) waits for a build (docs/specs/testflight-restart-wait.md): [`Running`] is held from the start of
//! one until it drops, one build at a time per repo.
pub mod build;
mod checks;
#[cfg(test)]
pub(crate) mod fake;
#[cfg(all(test, unix))]
mod real_tests;
mod setup;
#[cfg(test)]
mod tests;
mod tools;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

pub use build::{BUILD_LOG_FILE, LOG_FILE, LOG_KIND, Outcome, Result as BuildResult, check_and_build};
pub use checks::{Environment, check};
pub use setup::{build_number_at, build_script, pin_build_number, ready, setup_refusal, version_url_of, xcodeproj};
pub use tools::{Ran, RealTools, Step, Tools};

/// The repos with a build running now.
static BUILDING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// True while any TestFlight build runs.
pub fn testflight_running() -> bool {
    !BUILDING.lock().unwrap().is_empty()
}

/// A running build: held from its start until it drops (on error or panic too).
pub struct Running(PathBuf);

impl Running {
    /// Takes the repo's place, or `None` while another build of it runs: one at a time, a second is refused, not queued.
    pub fn take(repo: &Path) -> Option<Self> {
        let key = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        // Built only once the place is taken, and after the lock is let go: a guard made for a place that was refused
        // would free the holder's on drop.
        let taken = BUILDING.lock().unwrap().insert(key.clone());
        taken.then(|| Self(key))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        BUILDING.lock().unwrap().remove(&self.0);
    }
}
