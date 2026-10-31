//! A tiny local git repo with an `origin` remote, shared by this crate's own `#[cfg(test)]`
//! modules that need real git plumbing to exercise (repo checks, galleries). Also a baseline
//! `ProjectConfig` for the item-lifecycle state machine and its callers (`item_lifecycle`,
//! `orchestrator`), since the derived `Default` leaves every round/slice cap at 0, which would
//! escalate to `needs_human` on the very first failure or claim.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// Writes `contents` as an executable file a test can spawn at once.
///
/// On Linux a program can't be started while any process still holds it open for writing
/// (`ETXTBSY`, "Text file busy"). Another test thread forking at the wrong moment hands the child
/// our write descriptor until that child execs, so a spawn right after the write sometimes fails.
/// Once a spawn has succeeded nothing has the file open for writing and nothing will again, so
/// this runs the file once, retrying while it's busy, and tests can then spawn it freely. The fake
/// programs tests write are all harmless to run with no arguments.
pub(crate) fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Stdio;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        loop {
            match Command::new(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.wait();
                    return;
                }
                Err(e) if e.raw_os_error() == Some(26) && started.elapsed() < std::time::Duration::from_secs(10) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("starting {}: {e}", path.display()),
            }
        }
    }
}

/// Starts a long-lived process whose command line contains `dir`, so a scan for processes inside
/// it (`lanes::processes_in`) finds it. Shared by `lanes.rs`'s and `shutdown.rs`'s own tests.
pub(crate) fn hold_process(dir: &Path) -> std::process::Child {
    #[cfg(windows)]
    {
        let script = dir.join("hold.cmd");
        // Absolute path in the script body so CIM's CommandLine matches even when the
        // launcher rewrites argv.
        let marker = dir.to_string_lossy().replace('"', "");
        std::fs::write(
            &script,
            format!("@echo off\nrem retaskable-lane {marker}\n:loop\nping -n 60 127.0.0.1 >nul\ngoto loop\n"),
        )
        .unwrap();
        Command::new("cmd")
            .args(["/C", "call", script.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
    #[cfg(not(windows))]
    {
        let script = dir.join("hold.sh");
        std::fs::write(&script, "#!/bin/sh\nwhile true; do sleep 3600; done\n").unwrap();
        let _ = Command::new("chmod").args(["+x", script.to_str().unwrap()]).status();
        Command::new("sh")
            .arg(&script)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
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
