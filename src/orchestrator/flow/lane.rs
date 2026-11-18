//! A state that works in a lane can be entered from one that holds none (`awaiting_ci` to `checking`): the loop gives the item a
//! lane, which holds whatever ran there last. The step puts the item's pushed branch on it first (TS `prepareLane` on an
//! existing branch); a lane already on the branch is left as it is, with its uncommitted work.
use std::path::PathBuf;

use super::Ctx;

/// The lane's directory, on `branch`.
pub(super) async fn ensure(ctx: &Ctx, lane: u32, branch: Option<&str>) -> Result<PathBuf, String> {
    let dir = ctx.lane_dir(lane);
    let Some(branch) = branch.map(String::from) else {
        return Ok(dir);
    };
    let on = crate::lanes::git_output_in_worktree(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok();
    if on.is_some_and(|b| b.trim() == branch) {
        return Ok(dir);
    }
    let (repo, worktrees) = (
        PathBuf::from(&ctx.project.repo_path),
        PathBuf::from(&ctx.project.worktrees_dir),
    );
    let start = format!("origin/{branch}");
    let at = dir.clone();
    match tokio::task::spawn_blocking(move || crate::lanes::prepare_at(&repo, &worktrees, &at, &branch, &start)).await {
        Ok(Ok(_)) => Ok(dir),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(e.to_string()),
    }
}
