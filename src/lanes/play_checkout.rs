//! The project's one play checkout (`<worktreesDir>/play`; `preparePlay` and `playDir` in the TS agent, spec section 4.2): where a
//! build that is in no lane is played (**Play this build**), so it can be tried while every lane is busy and never holds up the
//! item's own steps. Always detached on the item's pushed branch, so the branch stays free for a work lane (git won't check
//! one branch out twice). `clean -fd` without `-x` keeps ignored build output, so later plays build incrementally.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{GIT_LOCK, ensure_worktree, fetch, git_in_worktree, kill_processes_in, link_outside_deps, lock, run};

/// `<worktrees_dir>/play`.
pub fn play_path(worktrees_dir: &Path) -> PathBuf {
    worktrees_dir.join("play")
}

/// Puts the play checkout on `branch`'s pushed tip (`origin/<branch>`, fetched first), detached; its folder.
pub fn prepare_play_checkout(repo_path: &Path, worktrees_dir: &Path, branch: &str) -> Result<PathBuf> {
    let dir = play_path(worktrees_dir);
    ensure_worktree(repo_path, worktrees_dir, &dir)?;
    kill_processes_in(&dir)?;
    {
        let _guard = lock(&GIT_LOCK);
        fetch(repo_path, false)?;
        run(git_in_worktree(&dir).args(["reset", "--hard"]))?;
        run(git_in_worktree(&dir).args(["clean", "-fd"]))?;
        run(git_in_worktree(&dir).args(["checkout", "--detach", &format!("origin/{branch}")]))?;
    }
    link_outside_deps(repo_path, worktrees_dir, &dir)?;
    Ok(dir)
}
