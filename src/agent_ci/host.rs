//! The host side of a CI check's fetch step (agent-ci.md section 3.2 step 1): a fresh directory
//! under the project's data folder holding the pull request's merge into main (or main alone for a
//! push), with a `.git` that carries no remote, credential helper or token, so the container that
//! mounts it has nothing to leak.
use crate::git_bin;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// What a check runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode<'a> {
    /// The pull request's head (a ref the remote serves, e.g. `refs/pull/7/head` or a branch)
    /// merged into main.
    PullRequest { head_ref: &'a str },
    /// Main alone.
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// The directory is ready, detached at this commit.
    Ready { dir: PathBuf, commit: String },
    /// The head doesn't merge into main: no container is started and the directory is already gone.
    Conflict { reason: String },
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = git_bin::command(dir)
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .output()
        .context("running git")?;
    if !output.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        bail!("{}", text.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Makes a fresh directory under the project's CI folder, clones `repo` (its object store) into it,
/// fetches main (and the head) from the repo's `origin` with the host's own credentials, merges and
/// detaches, then strips the clone's config. `main_branch` is e.g. `main`.
pub fn fetch(data_dir: &Path, project: &str, repo: &Path, main_branch: &str, mode: Mode) -> Result<FetchOutcome> {
    let root = crate::paths::ci_dir(data_dir, project);
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = root.join(format!("check-{}-{nanos}-{n}", std::process::id()));
    std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    match fetch_into(&dir, repo, main_branch, mode) {
        Ok(commit) => Ok(FetchOutcome::Ready { dir, commit }),
        Err(Failure::Conflict(reason)) => {
            cleanup(&dir);
            Ok(FetchOutcome::Conflict { reason })
        }
        Err(Failure::Other(e)) => {
            cleanup(&dir);
            Err(e)
        }
    }
}

enum Failure {
    Conflict(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure::Other(e)
    }
}

fn fetch_into(dir: &Path, repo: &Path, main_branch: &str, mode: Mode) -> Result<String, Failure> {
    let repo_str = repo.to_string_lossy();
    let dir_str = dir.to_string_lossy();
    // The URL goes on the command line, never into the clone's config.
    let url = git(repo, &["remote", "get-url", "origin"])?;
    git(
        dir,
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            "-c",
            "core.autocrlf=false",
            &repo_str,
            &dir_str,
        ],
    )?;
    git(dir, &["config", "core.autocrlf", "false"])?;
    let main_spec = format!("+refs/heads/{main_branch}:refs/retaskable/main");
    let mut args = vec!["fetch", "--quiet", "--no-tags", url.as_str(), main_spec.as_str()];
    let head_spec;
    if let Mode::PullRequest { head_ref } = mode {
        head_spec = format!("+{head_ref}:refs/retaskable/head");
        args.push(&head_spec);
    }
    git(dir, &args)?;
    git(dir, &["checkout", "--quiet", "--detach", "refs/retaskable/main"])?;
    if matches!(mode, Mode::PullRequest { .. }) {
        let merged = git(
            dir,
            &[
                "-c",
                "user.name=Retaskable CI",
                "-c",
                "user.email=ci@retaskable.invalid",
                "merge",
                "--no-ff",
                "--no-edit",
                "--quiet",
                "refs/retaskable/head",
            ],
        );
        if let Err(e) = merged {
            return Err(Failure::Conflict(e.to_string()));
        }
    }
    let commit = git(dir, &["rev-parse", "HEAD"])?;
    scrub(dir)?;
    Ok(commit)
}

/// Leaves `.git/config` with no remote, no `credential`, no `http` and no `url.*` sections, and the
/// fetch's working refs gone.
fn scrub(dir: &Path) -> Result<()> {
    for r in ["refs/retaskable/main", "refs/retaskable/head"] {
        let _ = git(dir, &["update-ref", "-d", r]);
    }
    let _ = git(dir, &["remote", "remove", "origin"]);
    let listed = git(dir, &["config", "--local", "--name-only", "--list"])?;
    let mut sections: Vec<String> = Vec::new();
    for key in listed.lines() {
        let mut parts = key.split('.');
        let first = parts.next().unwrap_or("");
        if matches!(first, "remote" | "credential" | "http" | "url" | "branch") {
            let section = match key.rsplit_once('.') {
                Some((s, _)) if key.matches('.').count() >= 2 => s.to_string(),
                _ => first.to_string(),
            };
            if !sections.contains(&section) {
                sections.push(section);
            }
        }
    }
    for section in sections {
        let _ = git(dir, &["config", "--local", "--remove-section", &section]);
    }
    Ok(())
}

/// Deletes a check's directory when it ends.
pub fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Deletes every directory left from a prior run (at start and on a restart's recovery).
pub fn sweep(data_dir: &Path, project: &str) {
    let root = crate::paths::ci_dir(data_dir, project);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            cleanup(&path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
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

    /// A bare origin with `main` (a.txt), a clone of it, and a branch `feature` pushed with b.txt.
    fn setup(root: &Path) -> PathBuf {
        let bare = root.join("origin.git");
        std::fs::create_dir_all(&bare).unwrap();
        sh(&bare, &["init", "--bare", "-b", "main"]);
        let repo = root.join("repo");
        sh(root, &["clone", bare.to_str().unwrap(), repo.to_str().unwrap()]);
        sh(&repo, &["checkout", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-m", "a"]);
        sh(&repo, &["push", "origin", "main"]);
        sh(&repo, &["checkout", "-b", "feature"]);
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-m", "b"]);
        sh(&repo, &["push", "origin", "feature"]);
        sh(&repo, &["checkout", "main"]);
        std::fs::write(repo.join("c.txt"), "c\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-m", "c"]);
        sh(&repo, &["push", "origin", "main"]);
        repo
    }

    fn fetch_pr(data: &Path, repo: &Path, head_ref: &str) -> Result<FetchOutcome> {
        fetch(data, "p", repo, "main", Mode::PullRequest { head_ref })
    }

    fn ready(outcome: FetchOutcome) -> (PathBuf, String) {
        match outcome {
            FetchOutcome::Ready { dir, commit } => (dir, commit),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pull_request_merge_has_both_parents_and_is_detached() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        let data = root.path().join("data");
        let out = fetch_pr(&data, &repo, "refs/heads/feature").unwrap();
        let (dir, commit) = ready(out);
        assert!(dir.starts_with(crate::paths::ci_dir(&data, "p")));
        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), commit);
        assert_eq!(
            sh(&dir, &["rev-list", "--parents", "-n1", "HEAD"]).split(' ').count(),
            3
        );
        assert!(sh(&dir, &["branch", "--show-current"]).is_empty());
        assert!(dir.join("b.txt").is_file() && dir.join("c.txt").is_file());
    }

    #[test]
    fn conflicting_head_returns_the_conflict_and_leaves_no_directory() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        sh(&repo, &["checkout", "-b", "clash", "HEAD~1"]);
        std::fs::write(repo.join("c.txt"), "other\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-m", "clash"]);
        sh(&repo, &["push", "origin", "clash"]);
        let data = root.path().join("data");
        let out = fetch_pr(&data, &repo, "refs/heads/clash").unwrap();
        assert!(
            matches!(out, FetchOutcome::Conflict { ref reason } if reason.contains("c.txt")),
            "{out:?}"
        );
        assert_eq!(std::fs::read_dir(crate::paths::ci_dir(&data, "p")).unwrap().count(), 0);
    }

    #[test]
    fn config_carries_no_remote_credential_or_token() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        sh(&repo, &["config", "credential.helper", "store"]);
        sh(
            &repo,
            &["config", "http.extraheader", "AUTHORIZATION: bearer FAKETOKEN123"],
        );
        sh(
            &repo,
            &[
                "config",
                "url.https://FAKETOKEN123@example.com/.insteadOf",
                "https://example.com/",
            ],
        );
        let data = root.path().join("data");
        let out = fetch_pr(&data, &repo, "refs/heads/feature").unwrap();
        let (dir, _) = ready(out);
        let config = std::fs::read_to_string(dir.join(".git").join("config")).unwrap();
        assert!(!config.contains("[remote"), "{config}");
        assert!(!config.contains("credential"), "{config}");
        assert!(!config.contains("extraheader"), "{config}");
        assert!(!config.contains("insteadOf"), "{config}");
        assert!(!config.contains("FAKETOKEN123"), "{config}");
        assert!(config.contains("autocrlf = false"), "{config}");
    }

    #[test]
    fn push_mode_checks_out_main_alone() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        let data = root.path().join("data");
        let (dir, commit) = ready(fetch(&data, "p", &repo, "main", Mode::Push).unwrap());
        assert_eq!(commit, sh(&repo, &["rev-parse", "main"]));
        assert!(dir.join("c.txt").is_file() && !dir.join("b.txt").exists());
        assert!(sh(&dir, &["branch", "--show-current"]).is_empty());
    }

    #[test]
    fn sweep_removes_leftovers_and_cleanup_removes_one() {
        let root = tempfile::tempdir().unwrap();
        let repo = setup(root.path());
        let data = root.path().join("data");
        let (a, _) = ready(fetch(&data, "p", &repo, "main", Mode::Push).unwrap());
        let (b, _) = ready(fetch(&data, "p", &repo, "main", Mode::Push).unwrap());
        cleanup(&a);
        assert!(!a.exists() && b.exists());
        let (_c, _) = ready(fetch(&data, "p", &repo, "main", Mode::Push).unwrap());
        sweep(&data, "p");
        assert_eq!(std::fs::read_dir(crate::paths::ci_dir(&data, "p")).unwrap().count(), 0);
    }
}
