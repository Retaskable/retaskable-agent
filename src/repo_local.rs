//! A local-only project: a git repo with no remote at all (`suggest`/`validate` in
//! `agent/src/setup.ts`). `repo::repo_info` and `repo::repo_problems` use these.

use std::path::Path;

use anyhow::{Context, Result};

use crate::repo::git;

/// Whether `git remote` lists at least one remote. As in TypeScript, a failing `git remote`
/// counts as having one.
pub fn has_remote(repo_path: &Path) -> bool {
    match git(repo_path).arg("remote").output() {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => true,
    }
}

/// A local-only repo's default branch: `main`, else `master`, else the checked-out branch.
pub fn local_default_branch(repo_path: &Path) -> Option<String> {
    for b in ["main", "master"] {
        if local_ref_exists(repo_path, &format!("refs/heads/{b}")) {
            return Some(b.to_string());
        }
    }
    let output = git(repo_path)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let b = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!b.is_empty()).then_some(b)
}

fn local_ref_exists(repo_path: &Path, spec: &str) -> bool {
    git(repo_path)
        .args(["rev-parse", "--verify", "--quiet", spec])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Whether the local branch has a commit (`validate`'s check of a local project's base branch).
pub fn local_branch_exists(repo_path: &Path, branch: &str) -> Result<bool> {
    let output = git(repo_path)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}^{{commit}}"),
        ])
        .output()
        .context("running git rev-parse")?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::{RepoCheck, repo_info, repo_problems};
    use crate::test_support::{git, run};

    fn local_repo(branch: &str, commit: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(git(dir.path()).args(["init", "-q"]));
        run(git(dir.path()).args(["checkout", "-q", "-b", branch]));
        if commit {
            run(git(dir.path()).args(["config", "user.email", "t@example.com"]));
            run(git(dir.path()).args(["config", "user.name", "T"]));
            run(git(dir.path()).args(["commit", "-q", "--allow-empty", "-m", "x"]));
        }
        dir
    }

    #[test]
    fn a_folder_with_no_remote_is_local_only() {
        for (branch, expected) in [("main", "main"), ("master", "master"), ("trunk", "trunk")] {
            let dir = local_repo(branch, true);
            let info = repo_info(dir.path());
            assert!(info.local_only);
            assert_eq!(info.origin_url, None);
            assert_eq!(info.code_host, None);
            assert_eq!(info.repo, None);
            assert_eq!(info.github_repo, None);
            assert_eq!(info.default_branch.as_deref(), Some(expected));
        }
    }

    #[test]
    fn a_remote_not_named_origin_is_not_local_only() {
        let dir = local_repo("main", true);
        run(git(dir.path()).args(["remote", "add", "upstream", "https://example.com/x.git"]));
        assert!(has_remote(dir.path()));
        assert!(!repo_info(dir.path()).local_only);
    }

    #[test]
    fn has_remote_follows_git_remote() {
        let dir = local_repo("main", false);
        assert!(!has_remote(dir.path()));
        run(git(dir.path()).args(["remote", "add", "origin", "https://example.com/x.git"]));
        assert!(has_remote(dir.path()));
    }

    #[test]
    fn local_branch_exists_needs_a_commit() {
        let committed = local_repo("main", true);
        assert!(local_branch_exists(committed.path(), "main").unwrap());
        assert!(!local_branch_exists(committed.path(), "nope").unwrap());
        let unborn = local_repo("main", false);
        assert!(!local_branch_exists(unborn.path(), "main").unwrap());
    }

    #[test]
    fn repo_problems_for_local_only() {
        let mut check = RepoCheck {
            is_git_repo: true,
            local_only: true,
            host_cli_authenticated: false,
            github_repo: None,
            backlog_on_origin: Some(false),
            integration_branch_error: Some("boom".to_string()),
            base_branch_exists: Some(true),
            ..Default::default()
        };
        assert!(repo_problems(&check, "main").is_empty());
        check.base_branch_exists = Some(false);
        assert_eq!(
            repo_problems(&check, "main"),
            vec!["There's no main branch in this repo yet. Commit something to it first."]
        );
    }
}
