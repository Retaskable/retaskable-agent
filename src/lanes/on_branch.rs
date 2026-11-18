//! A lane for an item that may already have a branch on the host. The TypeScript agent puts such an item's lane
//! back on its pushed branch (`reattachLane`) and starts from the base only when there is nothing to pick up
//! (`nothingToPickUp`); a lane that was freed while the item waited on CI would otherwise start again from the
//! base and push a branch that diverges from the host's copy.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{GIT_LOCK, fetch, lane_path, lock, prepare_at};

/// [`prepare`](super::prepare), except that the lane continues `origin/<branch>` when the host has that branch
/// with work the base doesn't: the work already pushed for the item is kept, not started over from the base.
pub fn prepare_on_branch(
    repo_path: &Path,
    worktrees_dir: &Path,
    lane: u32,
    branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let start = {
        let _guard = lock(&GIT_LOCK);
        fetch(repo_path, true)?;
        start_point(repo_path, branch, base_branch)
    };
    prepare_at(
        repo_path,
        worktrees_dir,
        &lane_path(worktrees_dir, lane),
        branch,
        &start,
    )
}

/// `origin/<branch>` when it exists and holds a commit `origin/<base>` doesn't; else `origin/<base>`.
fn start_point(repo_path: &Path, branch: &str, base_branch: &str) -> String {
    let base = format!("origin/{base_branch}");
    let pushed = format!("origin/{branch}");
    let ok = |args: &[&str]| {
        crate::git_bin::command(repo_path)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let exists = ok(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/remotes/{pushed}^{{commit}}"),
    ]);
    // Already in the base (the host's copy was merged and left in place): nothing of it to pick up.
    if exists && !ok(&["merge-base", "--is-ancestor", &pushed, &base]) {
        pushed
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=T"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repo with `main` pushed to an `origin`, and a worktrees folder with one lane.
    fn rig() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "--quiet", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "--quiet", "-m", "initial"]);
        let origin = root.path().join("origin.git");
        run_git(root.path(), &["clone", "--bare", "--quiet", "repo", "origin.git"]);
        run_git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
        run_git(&repo, &["push", "--quiet", "-u", "origin", "main"]);
        let worktrees = root.path().join("worktrees");
        super::super::ensure(&repo, &worktrees, 1).unwrap();
        (root, repo, worktrees)
    }

    /// Pushes a commit on `branch` (from main) to the origin, leaving the repo on main.
    fn push_branch(repo: &Path, branch: &str) -> String {
        run_git(repo, &["checkout", "--quiet", "-b", "tmp-push", "main"]);
        std::fs::write(repo.join("b.txt"), "b").unwrap();
        run_git(repo, &["add", "."]);
        run_git(repo, &["commit", "--quiet", "-m", "on the branch"]);
        run_git(
            repo,
            &["push", "--quiet", "origin", &format!("HEAD:refs/heads/{branch}")],
        );
        let sha = run_git(repo, &["rev-parse", "HEAD"]);
        run_git(repo, &["checkout", "--quiet", "main"]);
        run_git(repo, &["branch", "-D", "tmp-push"]);
        sha
    }

    #[test]
    fn a_branch_the_host_has_is_kept_not_started_over_from_the_base() {
        let (_root, repo, worktrees) = rig();
        let pushed = push_branch(&repo, "retaskable/item-s0");

        let lane = prepare_on_branch(&repo, &worktrees, 1, "retaskable/item-s0", "main").unwrap();

        assert_eq!(run_git(&lane, &["rev-parse", "HEAD"]), pushed);
        assert_eq!(run_git(&lane, &["branch", "--show-current"]), "retaskable/item-s0");
    }

    #[test]
    fn a_branch_the_host_lacks_starts_from_the_base() {
        let (_root, repo, worktrees) = rig();

        let lane = prepare_on_branch(&repo, &worktrees, 1, "retaskable/item-s0", "main").unwrap();

        assert_eq!(
            run_git(&lane, &["rev-parse", "HEAD"]),
            run_git(&repo, &["rev-parse", "main"])
        );
    }

    #[test]
    fn a_branch_already_in_the_base_starts_from_the_base() {
        let (_root, repo, worktrees) = rig();
        push_branch(&repo, "retaskable/item-s0");
        run_git(&repo, &["fetch", "--quiet", "origin"]);
        run_git(&repo, &["merge", "--quiet", "--ff-only", "origin/retaskable/item-s0"]);
        run_git(&repo, &["push", "--quiet", "origin", "main"]);

        let lane = prepare_on_branch(&repo, &worktrees, 1, "retaskable/item-s0", "main").unwrap();

        assert_eq!(
            run_git(&lane, &["rev-parse", "HEAD"]),
            run_git(&repo, &["rev-parse", "origin/main"])
        );
    }
}
