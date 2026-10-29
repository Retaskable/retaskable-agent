//! A tiny local git repo with an `origin` remote, shared by this crate's own `#[cfg(test)]`
//! modules that need real git plumbing to exercise (repo checks, galleries). Also a baseline
//! `ProjectConfig` for the item-lifecycle state machine and its callers (`item_lifecycle`,
//! `orchestrator`), since the derived `Default` leaves every round/slice cap at 0, which would
//! escalate to `needs_human` on the very first failure or claim.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::ProjectConfig;
use crate::triage::{KnownBug, TriageContext};

pub(crate) fn item_lifecycle_cfg() -> ProjectConfig {
    ProjectConfig {
        max_check_rounds: 3,
        max_review_rounds: 3,
        max_conflict_rounds: 5,
        max_slices_per_item: 10,
        ..Default::default()
    }
}

/// A `TriageContext` fixture shared by the triager's and the second look's tests (`timeline` and
/// `bug_title` vary between callers; everything else they need identical).
pub(crate) fn triage_context(timeline: Vec<String>, bug_title: &str) -> TriageContext {
    TriageContext {
        reason: "Review escalated: unclear which library to add.".to_string(),
        failed_in: "reviewing".to_string(),
        timeline,
        last_check_result: Some("passed".to_string()),
        pr_url: Some("https://github.com/x/y/pull/9".to_string()),
        branch: Some("retaskable/9-thing".to_string()),
        has_pushed_branch: true,
        has_session_to_continue: true,
        automatic_decisions: 1,
        transcripts_dir: Some(".retaskable/projects/x/runs/9".to_string()),
        known_bugs: vec![KnownBug {
            id: "retaskable-bug-abc123".to_string(),
            title: bug_title.to_string(),
        }],
    }
}

/// An env lookup that always reports the variable unset, for tests that pass a real one in.
pub(crate) fn no_env(_: &str) -> Option<String> {
    None
}

pub(crate) fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd
}

pub(crate) fn run(cmd: &mut Command) {
    let status = cmd.status().unwrap();
    assert!(status.success(), "{cmd:?} failed");
}

fn init_repo(dir: &Path, branch: &str) {
    run(git(dir).args(["init", "--quiet"]));
    run(git(dir).args(["config", "user.email", "test@example.com"]));
    run(git(dir).args(["config", "user.name", "Test"]));
    run(git(dir).args(["checkout", "-b", branch]));
    std::fs::write(dir.join("README.md"), "hello").unwrap();
    run(git(dir).args(["add", "README.md"]));
    run(git(dir).args(["commit", "--quiet", "-m", "initial"]));
}

/// A repo with an `origin` remote, both on `main`.
pub(crate) fn repo_with_origin(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo, "main");
    let origin = root.join("origin.git");
    run(Command::new("git")
        .args(["clone", "--bare", "--quiet"])
        .arg(&repo)
        .arg(&origin));
    run(git(&repo).args(["remote", "add", "origin"]).arg(&origin));
    run(git(&repo).args(["push", "-u", "--quiet", "origin", "main"]));
    repo
}
