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
        start_point(repo_path, branch, base_branch, false)
    };
    prepare_at(
        repo_path,
        worktrees_dir,
        &lane_path(worktrees_dir, lane),
        branch,
        &start,
    )
}

/// Makes sure the lane at `dir` is on the item's own `branch` before a session carries on there (`reattachLane`): a lane
/// already on it is left exactly as it is, uncommitted work included; one left on whatever another run did (another
/// item's branch, or none) is put on the branch, which keeps the commits not yet pushed and otherwise continues
/// `origin/<branch>`. The lane's own changes belong to the other run and are cleared, as `prepare` does.
pub fn ensure_on_branch(
    repo_path: &Path,
    worktrees_dir: &Path,
    dir: &Path,
    branch: &str,
    base_branch: &str,
) -> Result<()> {
    let on = super::git_output_in_worktree(dir, &["branch", "--show-current"]).unwrap_or_default();
    if on.trim() == branch {
        return Ok(());
    }
    let start = {
        let _guard = lock(&GIT_LOCK);
        fetch(repo_path, true)?;
        start_point(repo_path, branch, base_branch, true)
    };
    prepare_at(repo_path, worktrees_dir, dir, branch, &start).map(|_| ())
}

/// `origin/<branch>` when it exists and holds a commit `origin/<base>` doesn't; else `origin/<base>`. With `keep_local`,
/// the local `<branch>` first when it holds a commit neither the host's copy nor the base has (work not pushed yet).
fn start_point(repo_path: &Path, branch: &str, base_branch: &str, keep_local: bool) -> String {
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
    let local = format!("refs/heads/{branch}");
    if keep_local
        && ok(&["rev-parse", "--verify", "--quiet", &format!("{local}^{{commit}}")])
        && !ok(&["merge-base", "--is-ancestor", &local, &base])
        && !(exists && ok(&["merge-base", "--is-ancestor", &local, &pushed]))
    {
        return local;
    }
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
    fn a_lane_left_on_another_branch_is_put_back_on_the_items_with_its_pushed_work() {
        let (_root, repo, worktrees) = rig();
        let pushed = push_branch(&repo, "retaskable/item-s0");
        let lane = prepare_on_branch(&repo, &worktrees, 1, "retaskable/other-s0", "main").unwrap();
        std::fs::write(lane.join("dirt.txt"), "the other run's").unwrap();

        ensure_on_branch(&repo, &worktrees, &lane, "retaskable/item-s0", "main").unwrap();

        assert_eq!(run_git(&lane, &["branch", "--show-current"]), "retaskable/item-s0");
        assert_eq!(run_git(&lane, &["rev-parse", "HEAD"]), pushed);
        assert!(!lane.join("dirt.txt").exists());
    }

    #[test]
    fn a_commit_not_pushed_yet_on_the_items_branch_is_kept() {
        let (_root, repo, worktrees) = rig();
        push_branch(&repo, "retaskable/item-s0");
        let lane = prepare_on_branch(&repo, &worktrees, 1, "retaskable/item-s0", "main").unwrap();
        std::fs::write(lane.join("local.txt"), "mine").unwrap();
        run_git(&lane, &["add", "."]);
        run_git(&lane, &["commit", "--quiet", "-m", "unpushed"]);
        let unpushed = run_git(&lane, &["rev-parse", "HEAD"]);
        prepare_on_branch(&repo, &worktrees, 1, "retaskable/other-s0", "main").unwrap();

        ensure_on_branch(&repo, &worktrees, &lane, "retaskable/item-s0", "main").unwrap();

        assert_eq!(run_git(&lane, &["branch", "--show-current"]), "retaskable/item-s0");
        assert_eq!(run_git(&lane, &["rev-parse", "HEAD"]), unpushed);
    }

    #[test]
    fn a_lane_already_on_the_branch_keeps_its_uncommitted_work() {
        let (_root, repo, worktrees) = rig();
        push_branch(&repo, "retaskable/item-s0");
        let lane = prepare_on_branch(&repo, &worktrees, 1, "retaskable/item-s0", "main").unwrap();
        std::fs::write(lane.join("wip.txt"), "wip").unwrap();

        ensure_on_branch(&repo, &worktrees, &lane, "retaskable/item-s0", "main").unwrap();

        assert!(lane.join("wip.txt").exists());
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
