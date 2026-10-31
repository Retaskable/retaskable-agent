//! What a restart or Quit does to every session besides a detached implementer's
//! (docs/specs/agent/07.4-stopping-and-restarting.md, item `restart-cuts-sessions`): kills the
//! whole process tree running in each project's base checkout and main checkout, waiting for it
//! to actually go before the agent exits. A numbered lane is implementer territory and is left
//! alone here — a restart neither waits for those nor stops them.
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::ProjectConfig;
use crate::lanes;

/// How long to wait for a tree to go before reporting the one that stayed (docs/specs/agent/07.4,
/// "A graceful stop waits for the kills").
const KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Kills whatever's running in `dir`, waiting up to [`KILL_TIMEOUT`] for it to actually go.
fn stop_and_wait(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let deadline = Instant::now() + KILL_TIMEOUT;
    loop {
        match lanes::kill_processes_in(dir) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("{}: checking for processes to stop: {e:#}", dir.display());
                return;
            }
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                "{}: a process stayed running past the 5-second kill wait",
                dir.display()
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stops every other session (reviews, scouts, triage, drafts, chat, release notes, ideas, checks
/// and their installs, a shoot, a Play on phone build) still running in any project's main
/// checkout or base checkout (`lanes::base_checkout_path`), as a restart or Quit cuts them.
pub fn stop_other_sessions(projects: &[ProjectConfig]) {
    for project in projects {
        if !project.repo_path.is_empty() {
            stop_and_wait(Path::new(&project.repo_path));
        }
        if !project.worktrees_dir.is_empty() {
            stop_and_wait(&lanes::base_checkout_path(Path::new(&project.worktrees_dir)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::hold_process;

    #[test]
    fn stop_and_wait_returns_at_once_for_a_directory_with_nothing_running() {
        let dir = tempfile::tempdir().unwrap();

        let started = Instant::now();
        stop_and_wait(dir.path());

        // A single scan finding nothing returns right away, without ever sleeping and retrying;
        // a loaded CI machine can make that one scan slow, so this only rules out the retry loop
        // running all the way to KILL_TIMEOUT, rather than asserting a tight wall-clock bound.
        assert!(started.elapsed() < KILL_TIMEOUT);
    }

    #[test]
    fn stop_and_wait_returns_at_once_for_a_directory_that_doesnt_exist() {
        let missing = tempfile::tempdir().unwrap().path().join("nope");

        stop_and_wait(&missing);
    }

    #[test]
    fn stop_other_sessions_kills_a_process_in_the_base_checkout_but_leaves_a_lane_alone() {
        let root = tempfile::tempdir().unwrap();
        let worktrees = root.path().join("worktrees");
        let base = lanes::base_checkout_path(&worktrees);
        let lane = worktrees.join("lane-1");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&lane).unwrap();

        let mut in_base = hold_process(&base);
        let mut in_lane = hold_process(&lane);

        // stop_and_wait's own retry loop (up to 5s) covers a process the OS hasn't listed yet.
        let projects = [ProjectConfig {
            repo_path: String::new(),
            worktrees_dir: worktrees.to_string_lossy().into_owned(),
            ..Default::default()
        }];
        stop_other_sessions(&projects);

        let _ = in_base.wait();
        assert!(
            in_lane.try_wait().unwrap().is_none(),
            "a numbered lane must be left alone"
        );
        let _ = in_lane.kill();
        let _ = in_lane.wait();
    }
}
