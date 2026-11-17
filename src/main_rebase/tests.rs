use super::*;
use crate::test_support::{git, repo_with_origin, run as run_git};
use std::path::PathBuf;

fn commit(repo: &Path, name: &str, text: &str) {
    std::fs::write(repo.join(name), text).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

fn sha_of(repo: &Path, branch: &str) -> String {
    let out = git(repo)
        .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    base_sha: String,
    main_sha: String,
}

/// origin has `main` one commit ahead of `dev` (which has its own commit); `conflict` makes both touch one file.
fn fixture(conflict: bool) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "dev"]));
    commit(&repo, "c.txt", "dev side");
    run_git(git(&repo).args(["push", "--quiet", "origin", "dev"]));
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, if conflict { "c.txt" } else { "m.txt" }, "main side");
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    let (base_sha, main_sha) = (sha_of(&repo, "dev"), sha_of(&repo, "main"));
    Fixture {
        root,
        repo,
        base_sha,
        main_sha,
    }
}

fn go(f: &Fixture, base_sha: &str) -> Rebased {
    let wt = f.root.path().join("wt").join(WORKTREE);
    rebase_onto_main(&f.repo, &wt, "dev", "main", base_sha, &f.main_sha).unwrap()
}

fn parents(repo: &Path, rev: &str) -> usize {
    let out = git(repo)
        .args(["rev-list", "--parents", "-n", "1", rev])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().split_whitespace().count() - 1
}

#[test]
fn a_clean_rebase_puts_the_base_branch_on_top_of_main_with_no_merge_commit() {
    let f = fixture(false);
    assert_eq!(go(&f, &f.base_sha), Rebased::Done);
    let new = sha_of(&f.repo, "dev");
    assert_ne!(new, f.base_sha, "the branch was rewritten");
    run_git(git(&f.repo).args(["fetch", "--quiet", "origin"]));
    assert_eq!(parents(&f.repo, &new), 1, "a rebase, not a merge");
    let log = git(&f.repo).args(["log", "--format=%s", &new]).output().unwrap();
    let log = String::from_utf8(log.stdout).unwrap();
    assert!(
        log.starts_with("c.txt\nm.txt\n"),
        "dev's commit on top of main's: {log}"
    );
    assert_eq!(sha_of(&f.repo, "main"), f.main_sha, "main is never written");
    // The worktree is left detached at the base and no local branch remains.
    let branches = git(&f.repo).args(["branch", "--list", TMP]).output().unwrap();
    assert!(String::from_utf8(branches.stdout).unwrap().trim().is_empty());
}

#[test]
fn a_conflict_pushes_nothing_and_names_the_files_and_the_worktree_is_clean_for_the_next_try() {
    let f = fixture(true);
    assert_eq!(go(&f, &f.base_sha), Rebased::Conflict(vec!["c.txt".into()]));
    assert_eq!(sha_of(&f.repo, "dev"), f.base_sha);
    assert_eq!(
        go(&f, &f.base_sha),
        Rebased::Conflict(vec!["c.txt".into()]),
        "tried again from a clean worktree"
    );
}

#[test]
fn work_that_landed_since_the_head_was_read_is_never_thrown_away() {
    let f = fixture(false);
    // Someone pushes to dev after the head was read: the lease is on the head that was read.
    run_git(git(&f.repo).args(["checkout", "--quiet", "dev"]));
    commit(&f.repo, "late.txt", "late");
    run_git(git(&f.repo).args(["push", "--quiet", "origin", "dev"]));
    let late = sha_of(&f.repo, "dev");
    match go(&f, &f.base_sha) {
        Rebased::Failed(why) => assert!(why.starts_with("Couldn't push dev rebased onto main:"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(sha_of(&f.repo, "dev"), late, "their commit is still there");
}

#[test]
fn a_base_that_already_has_main_is_a_rebase_that_changes_nothing() {
    let f = fixture(false);
    assert_eq!(go(&f, &f.base_sha), Rebased::Done);
    let once = sha_of(&f.repo, "dev");
    assert_eq!(go(&f, &once), Rebased::Done);
    assert_eq!(sha_of(&f.repo, "dev"), once);
}

#[test]
fn only_the_integration_pattern_catches_up_by_a_rebase() {
    let mut p = crate::config::ProjectConfig::default();
    assert!(!uses_rebase(&p));
    p.github = Some(serde_json::json!({ "pattern": "squash" }));
    assert!(!uses_rebase(&p));
    p.github = Some(serde_json::json!({ "pattern": "integration" }));
    assert!(uses_rebase(&p));
}
