//! What a restart or Quit does to every session besides a detached implementer's
//! (docs/specs/agent/07.4-stopping-and-restarting.md, item `restart-cuts-sessions`): kills the
//! whole process tree running in each project's base checkout and main checkout, waiting for it
//! to actually go before the agent exits. A numbered lane is implementer territory and is left
//! alone here — a restart neither waits for those nor stops them.
//!
//! The main checkout is also the person's own working copy, so a scan there can't just match any
//! process whose command line happens to contain its path: that would as happily catch the
//! person's own dev server, a binary they built and are running from it, or an editor opened on
//! the folder. The kill is restricted to the agent's own descendants (`lanes::descendants_of`,
//! walked from this process), with anything still running in a numbered lane excluded even then
//! — a lane sitting inside the main checkout's `worktrees_dir` must not be cut here either.
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::ProjectConfig;
use crate::lanes;

/// How long to wait for a tree to go before reporting the one that stayed (docs/specs/agent/07.4,
/// "A graceful stop waits for the kills").
const KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Pids of every numbered lane's processes under `worktrees_dir`: a lane is implementer territory
/// and is left alone by a restart or Quit (above), including when its path also falls under the
/// main checkout being scanned (`worktrees_dir` nested inside `repo_path`).
fn lane_pids(worktrees_dir: &Path) -> HashSet<u32> {
    let mut pids = HashSet::new();
    let Ok(lanes_found) = lanes::existing_lane_numbers(worktrees_dir) else {
        return pids;
    };
    for lane in lanes_found {
        if let Ok(found) = lanes::processes_in(&lanes::lane_path(worktrees_dir, lane)) {
            pids.extend(found);
        }
    }
    pids
}

/// Kills whatever's running in `dir` that is both one of the agent's own (`own`) and not one of a
/// numbered lane's (`excluded_lane_pids`), waiting up to [`KILL_TIMEOUT`] for it to actually go.
fn stop_and_wait(dir: &Path, own: &HashSet<u32>, excluded_lane_pids: &HashSet<u32>) {
    if !dir.is_dir() {
        return;
    }
    let deadline = Instant::now() + KILL_TIMEOUT;
    loop {
        let pids = match lanes::processes_in(dir) {
            Ok(pids) => pids,
            Err(e) => {
                tracing::warn!("{}: checking for processes to stop: {e:#}", dir.display());
                return;
            }
        };
        let to_kill: Vec<u32> = pids
            .into_iter()
            .filter(|pid| own.contains(pid) && !excluded_lane_pids.contains(pid))
            .collect();
        if to_kill.is_empty() {
            return;
        }
        for pid in &to_kill {
            if let Err(e) = lanes::kill_tree(*pid) {
                tracing::warn!("{}: stopping pid {pid}: {e:#}", dir.display());
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
/// checkout or base checkout (`lanes::base_checkout_path`), as a restart or Quit cuts them. Only
/// the agent's own descendants are ever in scope (see the module docs above); a numbered lane's
/// processes are excluded even from those.
pub fn stop_other_sessions(projects: &[ProjectConfig]) {
    let own: HashSet<u32> = lanes::descendants_of(std::process::id()).into_iter().collect();
    for project in projects {
        let excluded_lane_pids = if project.worktrees_dir.is_empty() {
            HashSet::new()
        } else {
            lane_pids(Path::new(&project.worktrees_dir))
        };
        if !project.repo_path.is_empty() {
            stop_and_wait(Path::new(&project.repo_path), &own, &excluded_lane_pids);
        }
        if !project.worktrees_dir.is_empty() {
            stop_and_wait(
                &lanes::base_checkout_path(Path::new(&project.worktrees_dir)),
                &own,
                &excluded_lane_pids,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{hold_process, hold_unrelated_process, kill_unrelated_process};

    #[test]
    fn stop_and_wait_returns_at_once_for_a_directory_with_nothing_running() {
        let dir = tempfile::tempdir().unwrap();

        let started = Instant::now();
        stop_and_wait(dir.path(), &HashSet::new(), &HashSet::new());

        // A single scan finding nothing returns right away, without ever sleeping and retrying;
        // a loaded CI machine can make that one scan slow, so this only rules out the retry loop
        // running all the way to KILL_TIMEOUT, rather than asserting a tight wall-clock bound.
        assert!(started.elapsed() < KILL_TIMEOUT);
    }

    #[test]
    fn stop_and_wait_returns_at_once_for_a_directory_that_doesnt_exist() {
        let missing = tempfile::tempdir().unwrap().path().join("nope");

        stop_and_wait(&missing, &HashSet::new(), &HashSet::new());
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

    #[test]
    fn stop_other_sessions_leaves_a_process_in_the_main_checkout_alone_when_its_not_the_agents_own() {
        // The main checkout is the person's own working copy: a dev server, a binary they built,
        // or an editor opened on it can carry the folder's path on its command line without the
        // agent having started it. `hold_unrelated_process` stands in for one of those — it isn't
        // a descendant of this test process the way every one of the agent's own sessions is.
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        let pid = hold_unrelated_process(&repo);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !lanes::processes_in(&repo).unwrap().contains(&pid) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the unrelated process to appear in the process list"
            );
            std::thread::sleep(Duration::from_millis(200));
        }

        let projects = [ProjectConfig {
            repo_path: repo.to_string_lossy().into_owned(),
            worktrees_dir: String::new(),
            ..Default::default()
        }];
        stop_other_sessions(&projects);

        assert!(
            lanes::processes_in(&repo).unwrap().contains(&pid),
            "a process the agent never started must survive its scan of the main checkout"
        );
        kill_unrelated_process(pid);
    }
}
