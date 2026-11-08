//! Bringing a change up to date with its base without a session (docs/specs/pieces.md section 3.2,
//! the `merge` row; the throwaway-worktree update in docs/specs/agent/05-item-lifecycle.md's
//! `merging` row). Plain git, no model. `git` builds the command for a directory, so a test can
//! start plain `git` and production passes [`crate::git_bin::command`].

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::lanes::{fetch_is_stale, fetch_latest_with};
use crate::process_tree::output_std;

/// How to start git in a directory.
pub type Git<'a> = &'a dyn Fn(&Path) -> Command;

/// Whether the base has commits a change's head lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behind {
    Behind,
    UpToDate,
    /// The fetch failed, or the head isn't there.
    CantTell,
}

/// What a base update did. A failure is an `Err`, and pushes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// The branch now ends at this commit on origin.
    Updated { head: String },
    /// The merge conflicts in these files; nothing was pushed.
    Conflict { files: Vec<String> },
}

/// Fetches origin (skipped if one began in the last 15 seconds), then asks
/// `git merge-base --is-ancestor origin/<base> <head>`.
pub fn behind(git: Git, repo: &Path, base: &str, head: &str) -> Behind {
    if fetch_latest_with(git, repo).is_err() {
        return Behind::CantTell;
    }
    let status = output_std(
        git(repo)
            .args(["merge-base", "--is-ancestor"])
            .arg(format!("origin/{base}"))
            .arg(head),
    );
    // Exit 0: contained. 1: not an ancestor. Anything else (an unknown ref is 128): can't tell.
    match status.map(|o| o.status.code()) {
        Ok(Some(0)) => Behind::UpToDate,
        Ok(Some(1)) => Behind::Behind,
        _ => Behind::CantTell,
    }
}

pub fn worktree_path(worktrees_dir: &Path, piece: &str) -> PathBuf {
    worktrees_dir.join(format!("merge-{piece}"))
}

pub(crate) fn run(git: Git, dir: &Path, args: &[&str]) -> Result<String> {
    let output = output_std(
        git(dir)
            .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
            .args(args),
    )
    .context("running git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The throwaway worktree, removed when dropped (success, conflict, error or panic).
struct Throwaway<'a> {
    git: Git<'a>,
    repo: &'a Path,
    dir: PathBuf,
}

impl Throwaway<'_> {
    fn clear(&self) {
        let dir = self.dir.to_string_lossy();
        let _ = run(self.git, self.repo, &["worktree", "remove", "--force", &dir]);
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = run(self.git, self.repo, &["worktree", "prune"]);
    }
}

impl Drop for Throwaway<'_> {
    fn drop(&mut self) {
        self.clear();
    }
}

/// In `<worktreesDir>/merge-<piece>`, checks out the change's `branch` and merges `origin/<base>`
/// into it (a merge, not a rebase), then pushes it to the same branch, never forced. A conflict or
/// any failure pushes nothing. The worktree is removed afterwards either way.
pub fn update_from_base(
    git: Git,
    repo: &Path,
    worktrees_dir: &Path,
    piece: &str,
    branch: &str,
    base: &str,
) -> Result<Update> {
    // The base just moved, or the branch was just pushed: read both fresh.
    fetch_is_stale(repo);
    fetch_latest_with(git, repo)?;
    let guard = Throwaway {
        git,
        repo,
        dir: worktree_path(worktrees_dir, piece),
    };
    guard.clear();
    std::fs::create_dir_all(worktrees_dir).with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let dir = guard.dir.clone();
    run(
        git,
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &dir.to_string_lossy(),
            &format!("origin/{branch}"),
        ],
    )?;
    let merged = run(git, &dir, &["merge", "--no-edit", &format!("origin/{base}")]);
    if merged.is_err() {
        let files = run(git, &dir, &["diff", "--name-only", "--diff-filter=U"])?;
        let files: Vec<String> = files.lines().map(str::to_string).collect();
        if files.is_empty() {
            merged?;
        }
        return Ok(Update::Conflict { files });
    }
    let head = run(git, &dir, &["rev-parse", "HEAD"])?;
    if head != run(git, repo, &["rev-parse", &format!("origin/{branch}")])? {
        run(git, &dir, &["push", "origin", &format!("HEAD:refs/heads/{branch}")])?;
        fetch_is_stale(repo);
    }
    Ok(Update::Updated { head })
}

/// What the implementer is told when the update conflicts.
pub fn conflict_instruction(base: &str, files: &[String]) -> String {
    let list = files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
    format!(
        "Merge origin/{base} into your branch (a merge, not a rebase), resolve the conflicts, \
         commit and push. These files conflict:\n{list}"
    )
}

/// What a main sync's implementer is told when the update conflicts: it is merged with a merge commit, so never rebased.
pub fn sync_conflict_instruction(base: &str, files: &[String]) -> String {
    let list = files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
    format!(
        "Merge origin/{base} into your branch again (a merge, never a rebase: this sync lands with a merge commit), \
         resolve the conflicts, commit and push. These files conflict:\n{list}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{git as tgit, repo_with_origin, run as trun};

    fn plain(dir: &Path) -> Command {
        tgit(dir)
    }

    fn commit_file(repo: &Path, name: &str, text: &str) {
        std::fs::write(repo.join(name), text).unwrap();
        trun(plain(repo).args(["add", name]));
        trun(plain(repo).args(["commit", "--quiet", "-m", name]));
    }

    fn rev(repo: &Path, r: &str) -> String {
        let out = plain(repo).args(["rev-parse", r]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A repo whose origin has `main` and a PR branch `feat` off it, with local checkout on main.
    fn setup(root: &Path) -> PathBuf {
        let repo = repo_with_origin(root);
        trun(plain(&repo).args(["checkout", "--quiet", "-b", "feat"]));
        commit_file(&repo, "feat.txt", "feat");
        trun(plain(&repo).args(["push", "--quiet", "origin", "feat"]));
        trun(plain(&repo).args(["checkout", "--quiet", "main"]));
        repo
    }

    fn push_main(repo: &Path, name: &str, text: &str) {
        commit_file(repo, name, text);
        trun(plain(repo).args(["push", "--quiet", "origin", "main"]));
        fetch_is_stale(repo);
    }

    #[test]
    fn behind_reads_up_to_date_and_behind() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        assert_eq!(behind(&plain, &repo, "main", "origin/feat"), Behind::UpToDate);
        push_main(&repo, "m.txt", "m");
        assert_eq!(behind(&plain, &repo, "main", "origin/feat"), Behind::Behind);
        assert_eq!(behind(&plain, &repo, "main", "origin/nope"), Behind::CantTell);
    }

    #[test]
    fn clean_update_pushes_a_merge_commit_and_removes_the_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        push_main(&repo, "m.txt", "m");
        let wt = root.path().join("worktrees");
        let out = update_from_base(&plain, &repo, &wt, "p1", "feat", "main").unwrap();
        let Update::Updated { head } = out else {
            panic!("{out:?}")
        };
        trun(plain(&repo).args(["fetch", "--quiet", "origin"]));
        assert_eq!(rev(&repo, "origin/feat"), head);
        // A merge commit: two parents, and the old head is one of them (so not forced).
        assert_eq!(rev(&repo, &format!("{head}^2")), rev(&repo, "origin/main"));
        assert!(!worktree_path(&wt, "p1").exists());
        assert!(
            !String::from_utf8_lossy(&plain(&repo).args(["worktree", "list"]).output().unwrap().stdout)
                .contains("merge-p1")
        );
        assert_eq!(behind(&plain, &repo, "main", &head), Behind::UpToDate);
    }

    #[test]
    fn conflict_pushes_nothing_and_lists_the_files() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        trun(plain(&repo).args(["checkout", "--quiet", "feat"]));
        commit_file(&repo, "c.txt", "theirs");
        trun(plain(&repo).args(["push", "--quiet", "origin", "feat"]));
        trun(plain(&repo).args(["checkout", "--quiet", "main"]));
        push_main(&repo, "c.txt", "ours");
        let before = rev(&repo, "origin/feat");
        let wt = root.path().join("worktrees");
        let out = update_from_base(&plain, &repo, &wt, "p1", "feat", "main").unwrap();
        let Update::Conflict { files } = out else {
            panic!("{out:?}")
        };
        assert_eq!(files, vec!["c.txt".to_string()]);
        trun(plain(&repo).args(["fetch", "--quiet", "origin"]));
        assert_eq!(rev(&repo, "origin/feat"), before);
        assert!(!worktree_path(&wt, "p1").exists());
        let text = conflict_instruction("main", &files);
        assert!(text.contains("Merge origin/main into your branch") && text.contains("- c.txt"));
    }

    #[test]
    fn a_failure_part_way_leaves_no_worktree() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        let wt = root.path().join("worktrees");
        push_main(&repo, "m.txt", "m");
        // The push is the last step: a hook refusing every update to origin fails it after the merge.
        let hook = root.path().join("origin.git/hooks/pre-receive");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let err = update_from_base(&plain, &repo, &wt, "p1", "feat", "main");
        assert!(err.is_err());
        assert!(!worktree_path(&wt, "p1").exists());
        // An unknown branch fails before the merge.
        assert!(update_from_base(&plain, &repo, &wt, "p2", "missing", "main").is_err());
        assert!(!worktree_path(&wt, "p2").exists());
    }
}
