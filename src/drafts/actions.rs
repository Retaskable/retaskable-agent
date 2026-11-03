//! What a person does to a draft waiting for approval, on this computer (docs/specs/agent/06.5-spec-drafts.md
//! step 3): **Edit** a file by hand, and **Discard**. (Send back and the answers to questions come through the store's
//! claim, which resumes the drafter's session: `orchestrator::driver::drafts`.)
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};

use super::files::{DraftFile, draft_files};
use crate::draft_worktree::{create, git, remove};

/// Where a project's drafts live.
pub struct Where<'a> {
    pub repo_path: &'a Path,
    pub worktrees_dir: &'a Path,
    pub base_branch: &'a str,
}

/// The draft's branch: the one named, or the one `retaskable/spec-<id>-…` in the checkout.
pub fn branch_of(w: &Where<'_>, id: &str, named: Option<&str>) -> Result<String> {
    if let Some(b) = named.map(str::trim).filter(|b| !b.is_empty()) {
        return Ok(b.to_string());
    }
    let pattern = format!("refs/heads/retaskable/spec-{id}-*");
    let found = git(w.repo_path, &["for-each-ref", "--format=%(refname:short)", &pattern])?;
    match found
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()[..]
    {
        [one] => Ok(one.to_string()),
        [] => bail!("no branch for draft {id} on this computer"),
        _ => bail!("more than one branch for draft {id}; name it"),
    }
}

/// Writes `content` to `rel` in the draft's worktree (made again from its branch if it was removed), commits it,
/// pushes it to the same branch, and lists the draft's files again.
pub fn edit(w: &Where<'_>, id: &str, branch: &str, rel: &str, content: &str) -> Result<Vec<DraftFile>> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || !rel_path.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("{rel:?} isn't a path inside the draft");
    }
    let dir = create(w.repo_path, w.worktrees_dir, id, branch, w.base_branch)?;
    let file = dir.join(rel_path);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&file, content).with_context(|| format!("writing {}", file.display()))?;
    git(&dir, &["add", "--", rel])?;
    if git(&dir, &["diff", "--cached", "--quiet"]).is_err() {
        git(&dir, &["commit", "--quiet", "-m", &format!("Edit {rel}")])?;
        git(&dir, &["push", "origin", &format!("HEAD:refs/heads/{branch}")])?;
    }
    draft_files(&dir, w.base_branch)
}

/// Closes the draft's PR with `gh` and deletes its branch, then removes the worktree.
pub fn discard(w: &Where<'_>, gh: &Path, id: &str, branch: &str) -> Result<()> {
    remove(w.repo_path, w.worktrees_dir, id)?;
    let out = std::process::Command::new(gh)
        .current_dir(w.repo_path)
        .args(["pr", "close", branch, "--delete-branch"])
        .output()
        .context("starting gh pr close")?;
    if !out.status.success() {
        bail!("gh pr close failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    // gh drops the local branch only when it can; a branch it left is deleted here.
    let _ = git(w.repo_path, &["branch", "-D", branch]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_worktree::path;
    use crate::test_support::{git as tgit, repo_with_origin, run, write_executable};

    fn setup(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let repo = repo_with_origin(root);
        let worktrees = root.join("worktrees");
        // A draft the drafter wrote and published.
        let dir = create(&repo, &worktrees, "7", "retaskable/spec-7-x", "main").unwrap();
        std::fs::write(dir.join("a.md"), "one\n").unwrap();
        git(&dir, &["add", "a.md"]).unwrap();
        git(&dir, &["commit", "--quiet", "-m", "draft"]).unwrap();
        git(&dir, &["push", "origin", "HEAD:refs/heads/retaskable/spec-7-x"]).unwrap();
        (repo, worktrees)
    }

    #[test]
    fn an_edit_is_committed_pushed_and_listed_again() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, worktrees) = setup(tmp.path());
        let w = Where {
            repo_path: &repo,
            worktrees_dir: &worktrees,
            base_branch: "main",
        };
        assert_eq!(branch_of(&w, "7", None).unwrap(), "retaskable/spec-7-x");
        let files = edit(&w, "7", "retaskable/spec-7-x", "a.md", "two\n").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "two\n");
        let origin = tmp.path().join("origin.git");
        let shown = git(&origin, &["show", "retaskable/spec-7-x:a.md"]).unwrap();
        assert_eq!(shown, "two\n");
        let log = git(&origin, &["log", "--format=%s", "retaskable/spec-7-x"]).unwrap();
        assert!(log.starts_with("Edit a.md"), "{log}");
        // The same content again commits nothing.
        edit(&w, "7", "retaskable/spec-7-x", "a.md", "two\n").unwrap();
        assert_eq!(
            git(&origin, &["rev-list", "--count", "retaskable/spec-7-x"])
                .unwrap()
                .trim(),
            "3"
        );
    }

    #[test]
    fn an_edit_outside_the_draft_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, worktrees) = setup(tmp.path());
        let w = Where {
            repo_path: &repo,
            worktrees_dir: &worktrees,
            base_branch: "main",
        };
        for bad in ["", "../x.md", "/etc/x", "a/../../x"] {
            assert!(edit(&w, "7", "retaskable/spec-7-x", bad, "x").is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn discard_closes_the_pr_deletes_the_branch_and_removes_the_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, worktrees) = setup(tmp.path());
        let log = tmp.path().join("gh.log");
        let gh = tmp.path().join("gh");
        write_executable(&gh, &format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()));
        std::fs::remove_file(&log).ok();
        let w = Where {
            repo_path: &repo,
            worktrees_dir: &worktrees,
            base_branch: "main",
        };
        discard(&w, &gh, "7", "retaskable/spec-7-x").unwrap();
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            "pr close retaskable/spec-7-x --delete-branch"
        );
        assert!(!path(&worktrees, "7").exists());
        run(tgit(&repo).args(["rev-parse", "--verify", "--quiet", "main"]));
        assert!(
            git(
                &repo,
                &["show-ref", "--verify", "--quiet", "refs/heads/retaskable/spec-7-x"]
            )
            .is_err()
        );
    }
}
