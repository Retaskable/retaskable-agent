//! The pool of git worktrees ("lanes") a project's sessions work in, one at a time
//! (docs/specs/agent/04-architecture.md section 4.1). A lane is `<worktreesDir>/lane-<k>`
//! (k = 1..=the computer's `lanes`), made once with `git worktree add --detach` and kept
//! between items so its build output stays warm; the orchestrator never makes a fresh worktree
//! per item. `ensure` reconciles the worktrees on disk for one project to the wanted count.
//! Handing a lane to one session at a time is for a later piece, once there's a session to hand
//! one to.
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// What `ensure` changed, for logging.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub created: Vec<u32>,
    pub removed: Vec<u32>,
}

/// Reconciles `<worktrees_dir>/lane-<k>` for k = 1..=`lane_count` to exist as worktrees of
/// `repo_path`, creating what's missing and removing anything numbered higher than that
/// ("raising lanes adds worktrees on demand. Lowering it removes the highest-numbered lanes once
/// they're free"). A lane folder left behind with no `.git` (a worktree remove that failed
/// part-way, e.g. a locked file on Windows) is deleted before a lane is made there again
/// ("No half-removed lanes"). Safe to call repeatedly: an already-reconciled pool is left as it
/// is.
pub fn ensure(repo_path: &Path, worktrees_dir: &Path, lane_count: u32) -> Result<Reconciled> {
    std::fs::create_dir_all(worktrees_dir).with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let mut result = Reconciled::default();
    for lane in 1..=lane_count {
        let path = lane_path(worktrees_dir, lane);
        if is_worktree(&path) {
            continue;
        }
        if path.exists() {
            std::fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        add_worktree(repo_path, &path)?;
        result.created.push(lane);
    }
    for lane in existing_lane_numbers(worktrees_dir)? {
        if lane <= lane_count {
            continue;
        }
        let path = lane_path(worktrees_dir, lane);
        if is_worktree(&path) {
            if let Err(e) = remove_worktree(repo_path, &path) {
                tracing::warn!("removing lane {lane} at {}: {e:#}", path.display());
                continue;
            }
        } else {
            std::fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        result.removed.push(lane);
    }
    Ok(result)
}

fn lane_path(worktrees_dir: &Path, lane: u32) -> PathBuf {
    worktrees_dir.join(format!("lane-{lane}"))
}

fn is_worktree(path: &Path) -> bool {
    path.join(".git").exists()
}

fn existing_lane_numbers(worktrees_dir: &Path) -> Result<Vec<u32>> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(worktrees_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e).with_context(|| format!("reading {}", worktrees_dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        if let Some(lane) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("lane-"))
            .and_then(|num| num.parse::<u32>().ok())
        {
            found.push(lane);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn git(repo_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd
}

fn run(cmd: &mut Command) -> Result<()> {
    let output = cmd.output().context("running git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn add_worktree(repo_path: &Path, lane_path: &Path) -> Result<()> {
    run(git(repo_path).args(["worktree", "add", "--detach"]).arg(lane_path))
}

fn remove_worktree(repo_path: &Path, lane_path: &Path) -> Result<()> {
    run(git(repo_path).args(["worktree", "remove", "--force"]).arg(lane_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare repo with one commit, so `git worktree add` has a HEAD to check out.
    fn init_repo(dir: &Path) {
        run(git(dir).args(["init", "--quiet"])).unwrap();
        run(git(dir).args(["config", "user.email", "test@example.com"])).unwrap();
        run(git(dir).args(["config", "user.name", "Test"])).unwrap();
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        run(git(dir).args(["add", "README.md"])).unwrap();
        run(git(dir).args(["commit", "--quiet", "-m", "initial"])).unwrap();
    }

    fn worktree_list(repo: &Path) -> String {
        let output = git(repo).args(["worktree", "list"]).output().unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn ensure_creates_the_wanted_number_of_lanes() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();

        let result = ensure(repo.path(), worktrees.path(), 3).unwrap();

        assert_eq!(result.created, vec![1, 2, 3]);
        assert!(result.removed.is_empty());
        for lane in 1..=3 {
            assert!(is_worktree(&lane_path(worktrees.path(), lane)));
        }
        assert_eq!(worktree_list(repo.path()).lines().count(), 4); // the repo itself plus 3 lanes
    }

    #[test]
    fn ensure_is_a_no_op_once_the_pool_already_matches() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 2).unwrap();

        let second = ensure(repo.path(), worktrees.path(), 2).unwrap();

        assert_eq!(second, Reconciled::default());
    }

    #[test]
    fn lowering_the_count_removes_the_highest_numbered_lanes_once_theyre_free() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 4).unwrap();

        let result = ensure(repo.path(), worktrees.path(), 2).unwrap();

        assert_eq!(result.removed, vec![3, 4]);
        assert!(!lane_path(worktrees.path(), 3).exists());
        assert!(!lane_path(worktrees.path(), 4).exists());
        assert!(lane_path(worktrees.path(), 1).exists());
        assert!(lane_path(worktrees.path(), 2).exists());
    }

    #[test]
    fn raising_the_count_again_recreates_the_lanes_it_had_removed() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 3).unwrap();
        ensure(repo.path(), worktrees.path(), 1).unwrap();

        let result = ensure(repo.path(), worktrees.path(), 3).unwrap();

        assert_eq!(result.created, vec![2, 3]);
        assert!(is_worktree(&lane_path(worktrees.path(), 2)));
        assert!(is_worktree(&lane_path(worktrees.path(), 3)));
    }

    #[test]
    fn a_half_removed_lane_folder_is_deleted_before_its_lane_is_made_again() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        let stray = lane_path(worktrees.path(), 2);
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("leftover.txt"), "junk").unwrap();

        let result = ensure(repo.path(), worktrees.path(), 2).unwrap();

        assert_eq!(result.created, vec![1, 2]);
        assert!(is_worktree(&stray));
        assert!(!stray.join("leftover.txt").exists());
    }

    #[test]
    fn a_zero_count_removes_every_lane() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let worktrees = tempfile::tempdir().unwrap();
        ensure(repo.path(), worktrees.path(), 2).unwrap();

        let result = ensure(repo.path(), worktrees.path(), 0).unwrap();

        assert_eq!(result.removed, vec![1, 2]);
        assert_eq!(worktree_list(repo.path()).lines().count(), 1);
    }
}
