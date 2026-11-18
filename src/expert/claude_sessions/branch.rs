//! A continuing session's branch (docs/specs/expert-chat.md section 3.10): `retaskable/<id>-<slug>` starts at the tip
//! of the branch the Claude Code session was on, when that folder is a checkout of this project, so the session's
//! lane picks that work up as a parked session's does.
use std::path::Path;

use crate::git_bin::command;
use crate::merge_update::run as git;

/// A branch name that is safe to put in a refspec: none of the characters git refuses in one, no `..`, and not an
/// option. The name comes from a file the person's Claude Code wrote, so it is checked rather than trusted.
fn safe_ref(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/'])
        && !name.contains("..")
        && !name.contains("@{")
        && !name.chars().any(|c| c.is_control() || " ~^:?*[\\".contains(c))
}

/// The git directory and origin of the checkout at `dir`, to tell whether two folders hold the same project.
fn whose(dir: &Path) -> (Option<String>, Option<String>) {
    let out = |args: &[&str]| git(&command, dir, args).ok().filter(|s| !s.is_empty());
    let common = out(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let origin = out(&["remote", "get-url", "origin"]);
    (
        common.map(|c| c.trim_end_matches(['/', '\\']).to_lowercase()),
        origin.map(|o| o.trim_end_matches(['/', '\\']).trim_end_matches(".git").to_lowercase()),
    )
}

/// Starts branch `name` at the tip of `branch` in `folder` when that folder is a checkout of this project (its
/// repository, a worktree of it or another clone of its origin) on a branch other than `base`: fetched into
/// `repo` and pushed, so the lane finds it on `origin`. Why it didn't, or `Ok` when it did.
pub fn branch_at_tip(repo: &Path, base: &str, folder: &Path, branch: Option<&str>, name: &str) -> Result<(), String> {
    let branch = match branch {
        None | Some("HEAD") => return Err("its folder wasn't on a branch".into()),
        Some(b) => b,
    };
    if branch == base {
        return Err(format!("it worked on {branch} itself"));
    }
    if !folder.is_dir() {
        return Err("its folder is gone".into());
    }
    let (here, there) = (whose(repo), whose(folder));
    let same = (here.0.is_some() && here.0 == there.0) || (here.1.is_some() && here.1 == there.1);
    if !same {
        return Err("its folder isn't a checkout of this project".into());
    }
    if !safe_ref(branch) {
        return Err(format!("its branch name {branch:?} isn't one to copy"));
    }
    let copied = || -> anyhow::Result<()> {
        let from = folder.to_string_lossy();
        git(
            &command,
            repo,
            &[
                "fetch",
                "--no-tags",
                &from,
                &format!("+refs/heads/{branch}:refs/heads/{name}"),
            ],
        )?;
        git(
            &command,
            repo,
            &["push", "origin", &format!("refs/heads/{name}:refs/heads/{name}")],
        )?;
        Ok(())
    };
    copied().map_err(|e| format!("its branch couldn't be copied ({e:#})"))
}
