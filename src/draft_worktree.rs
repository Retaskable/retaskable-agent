//! A draft's docs-only worktree (docs/specs/agent/06.5-spec-drafts.md step 1):
//! `<worktreesDir>/draft-<id>` on branch `retaskable/spec-<id>-<slug>`, from `origin/<baseBranch>`.
//! It is only ever checked out and removed: no build output is made there and no dependencies are
//! linked in.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const SLUG_MAX: usize = 40;

/// A short lowercase hyphenated slug from a request's first title or text: runs of anything that
/// isn't a letter or digit become one hyphen, at most 40 characters, never starting or ending in
/// a hyphen. Text with no letters or digits gives `draft`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut cut: String = out.chars().take(SLUG_MAX).collect();
    while cut.ends_with('-') {
        cut.pop();
    }
    if cut.is_empty() { "draft".to_string() } else { cut }
}

pub fn branch_name(id: &str, slug: &str) -> String {
    format!("retaskable/spec-{id}-{slug}")
}

pub fn path(worktrees_dir: &Path, id: &str) -> PathBuf {
    worktrees_dir.join(format!("draft-{id}"))
}

/// Git inside the project's checkout, with the agent's own calls skipping `maintenance run --auto`
/// (04-architecture.md section 4.1.1).
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let mut cmd: Command = crate::git_bin::command(dir);
    let output = cmd
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .output()
        .context("running git")?;
    if !output.status.success() {
        bail!("git failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Makes the draft's worktree on `branch` from `origin/<base_branch>` and returns its directory.
/// A restart reuses the worktree if it is there, and the branch (with whatever it holds) if only
/// the worktree was removed.
pub fn create(repo_path: &Path, worktrees_dir: &Path, id: &str, branch: &str, base_branch: &str) -> Result<PathBuf> {
    let dir = path(worktrees_dir, id);
    if dir.join(".git").exists() {
        return Ok(dir);
    }
    crate::lanes::fetch_latest(repo_path)?;
    git(repo_path, &["worktree", "prune"])?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    std::fs::create_dir_all(worktrees_dir).with_context(|| format!("creating {}", worktrees_dir.display()))?;
    let dir_arg = dir.to_string_lossy();
    let branch_ref = format!("refs/heads/{branch}");
    if git(repo_path, &["show-ref", "--verify", "--quiet", &branch_ref]).is_ok() {
        git(repo_path, &["worktree", "add", &dir_arg, branch])?;
    } else {
        let start = format!("origin/{base_branch}");
        git(
            repo_path,
            &["worktree", "add", "--no-track", "-b", branch, &dir_arg, &start],
        )?;
    }
    Ok(dir)
}

/// Removes the draft's worktree and prunes it; the branch stays, for the PR.
pub fn remove(repo_path: &Path, worktrees_dir: &Path, id: &str) -> Result<()> {
    let dir = path(worktrees_dir, id);
    if dir.join(".git").exists() {
        git(repo_path, &["worktree", "remove", "--force", &dir.to_string_lossy()])?;
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    git(repo_path, &["worktree", "prune"])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap()
    }

    fn init_with_origin(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "--quiet"]);
        sh(&repo, &["config", "user.email", "test@example.com"]);
        sh(&repo, &["config", "user.name", "Test"]);
        sh(&repo, &["checkout", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "hello").unwrap();
        sh(&repo, &["add", "README.md"]);
        sh(&repo, &["commit", "--quiet", "-m", "initial"]);
        let origin = root.join("origin.git");
        sh(
            &repo,
            &[
                "clone",
                "--bare",
                "--quiet",
                &repo.to_string_lossy(),
                &origin.to_string_lossy(),
            ],
        );
        sh(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        sh(&repo, &["push", "-u", "--quiet", "origin", "main"]);
        repo
    }

    #[test]
    fn slug_handles_punctuation_unicode_and_long_text() {
        assert_eq!(slug("Add a  Dark-Mode toggle!"), "add-a-dark-mode-toggle");
        assert_eq!(slug("  --Hello, World?!  "), "hello-world");
        assert_eq!(slug("Café Ünïcode 日本語"), "café-ünïcode-日本語");
        assert_eq!(slug("!!! ???"), "draft");
        assert_eq!(slug(""), "draft");
        let long = slug(&"word ".repeat(50));
        assert!(long.chars().count() <= SLUG_MAX);
        assert!(!long.ends_with('-'));
        assert_eq!(slug(&format!("{}x y", "a".repeat(39))), format!("{}x", "a".repeat(39)));
        assert_eq!(slug(&format!("{} y", "a".repeat(39))), "a".repeat(39));
    }

    #[test]
    fn branch_name_joins_id_and_slug() {
        assert_eq!(branch_name("7", "dark-mode"), "retaskable/spec-7-dark-mode");
    }

    #[test]
    fn create_makes_the_worktree_at_origin_base_and_reuses_it() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_with_origin(root.path());
        let worktrees = root.path().join("wt");
        let branch = branch_name("7", "dark-mode");

        let dir = create(&repo, &worktrees, "7", &branch, "main").unwrap();

        assert_eq!(dir, worktrees.join("draft-7"));
        assert_eq!(sh(&dir, &["branch", "--show-current"]).trim(), branch);
        assert_eq!(
            sh(&dir, &["rev-parse", "HEAD"]),
            sh(&repo, &["rev-parse", "origin/main"])
        );

        std::fs::write(dir.join("spec.md"), "draft").unwrap();
        let again = create(&repo, &worktrees, "7", &branch, "main").unwrap();
        assert_eq!(again, dir);
        assert!(dir.join("spec.md").exists());
    }

    #[test]
    fn create_after_remove_reuses_the_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_with_origin(root.path());
        let worktrees = root.path().join("wt");
        let branch = branch_name("7", "x");
        let dir = create(&repo, &worktrees, "7", &branch, "main").unwrap();
        std::fs::write(dir.join("spec.md"), "draft").unwrap();
        sh(&dir, &["add", "spec.md"]);
        sh(&dir, &["commit", "--quiet", "-m", "spec"]);
        let tip = sh(&dir, &["rev-parse", "HEAD"]);

        remove(&repo, &worktrees, "7").unwrap();
        let dir = create(&repo, &worktrees, "7", &branch, "main").unwrap();

        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), tip);
    }

    #[test]
    fn remove_deletes_the_directory_prunes_it_and_keeps_the_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = init_with_origin(root.path());
        let worktrees = root.path().join("wt");
        let branch = branch_name("7", "x");
        let dir = create(&repo, &worktrees, "7", &branch, "main").unwrap();

        remove(&repo, &worktrees, "7").unwrap();

        assert!(!dir.exists());
        assert!(!sh(&repo, &["worktree", "list"]).contains("draft-7"));
        assert!(sh(&repo, &["branch", "--list", &branch]).contains(&branch));
        remove(&repo, &worktrees, "7").unwrap(); // already gone
    }
}
