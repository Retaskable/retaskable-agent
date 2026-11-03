//! `watch` runs against a fake `gh` and a repo whose base branch has a workflow.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::backlog::Client;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::{RunContext, RunEndSender, RunProcesses};
use crate::runs::RunLocks;
use crate::test_support::write_executable;

/// A git repo whose `origin/main` has a workflow that runs on pull requests, so CI counts as covering the base.
pub(crate) fn repo_with_ci(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    std::fs::write(
        repo.join(".github/workflows/ci.yml"),
        "on:\n  pull_request:\njobs:\n  t:\n    runs-on: x\n",
    )
    .unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "."],
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "ci"],
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    ] {
        let ok = std::process::Command::new("git").args(args).current_dir(&repo).status();
        assert!(ok.unwrap().success());
    }
    repo
}

/// A fake `gh` (unix): `pr checks` prints `checks` and exits `code`, or fails when `error` exists or hangs when
/// `hang` does; `run view` prints `log`; every call is logged to `calls.log`.
pub(crate) fn fake_gh(dir: &Path) -> PathBuf {
    let path = dir.join("fake_gh.sh");
    let script = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/calls.log"
case "$*" in
  *--json*) echo '[]';;
  "run view "*) cat "$d/log";;
  "pr checks "*)
    [ -f "$d/hang" ] && sleep 30
    if [ -f "$d/error" ]; then echo boom >&2; exit 1; fi
    cat "$d/checks"
    exit $(cat "$d/code" 2>/dev/null || echo 0);;
esac
"#;
    write_executable(&path, script);
    path
}

pub(crate) fn set(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

pub(crate) fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty()) // `write_executable` runs the script once with nothing
        .map(String::from)
        .collect()
}

/// A run with no lane against the store at `url`; its process registrations are kept open (leaked) so a registered
/// tree isn't cut as the loop's gone.
pub(crate) fn lane_free_request(
    token: &str,
    kind: RunKind,
    piece: crate::backlog::runs::RunPiece,
    config: ProjectConfig,
    url: &str,
    ends: RunEndSender,
) -> RunRequest {
    let (processes, processes_rx) = RunProcesses::channel();
    std::mem::forget(processes_rx);
    RunRequest {
        token: token.into(),
        kind,
        piece,
        lane: None,
        ends,
        project: "p".into(),
        processes,
        stop: Default::default(),
        context: RunContext {
            project: "p".into(),
            config,
            client: std::sync::Arc::new(Client::new(url)),
            repo: 1,
            token: String::new(),
            locks: RunLocks::default(),
        },
    }
}

fn request(repo: &Path, change: Value, max_minutes: u32) -> (RunRequest, tokio::sync::mpsc::UnboundedReceiver<RunEnd>) {
    let (ends, rx) = RunEndSender::channel();
    let mut config = ProjectConfig {
        repo_path: repo.display().to_string(),
        base_branch: "main".into(),
        ..Default::default()
    };
    config.tuning.ci_max_minutes = max_minutes;
    let piece = serde_json::from_value(json!({ "title": "t", "change": change })).unwrap();
    let request = lane_free_request("W1", RunKind::Watch, piece, config, "http://127.0.0.1:1", ends);
    (request, rx)
}

async fn run(dir: &Path, change: Value, max_minutes: u32) -> RunEnd {
    let repo = repo_with_ci(dir);
    let (request, mut rx) = request(&repo, change, max_minutes);
    let runner = WatchRuns::with(fake_gh(dir), Duration::from_millis(5), Some(Duration::from_millis(1)));
    runner.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .expect("the run ended")
        .unwrap()
}

#[test]
fn it_runs_watch_and_only_watch() {
    assert_eq!(WatchRuns::new().kinds(), [RunKind::Watch]);
}

#[cfg(unix)]
#[tokio::test]
async fn passing_checks_end_green_after_pending_ones_settle() {
    let dir = tempfile::tempdir().unwrap();
    set(dir.path(), "checks", "t\tpending\t0\thttps://x/t\t\n");
    set(dir.path(), "code", "8");
    let d = dir.path().to_path_buf();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        set(&d, "checks", "t\tpass\t1s\thttps://x/t\t\n");
        set(&d, "code", "0");
    });
    let end = run(dir.path(), json!({ "number": 7 }), 120).await;
    assert_eq!((end.token.as_str(), end.outcome), ("W1", RunOutcome::Green));
    assert!(calls(dir.path()).len() > 1, "it read more than once");
    assert_eq!(calls(dir.path())[0], "pr checks 7");
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_check_ends_red_with_its_name_link_and_log_tail() {
    let dir = tempfile::tempdir().unwrap();
    let link = "https://github.com/o/r/actions/runs/42/job/9";
    set(
        dir.path(),
        "checks",
        &format!("ok\tpass\t1s\thttps://x/ok\t\nbuild\tfail\t1s\t{link}\t\n"),
    );
    set(dir.path(), "code", "8");
    set(dir.path(), "log", "one\ntwo\n");
    let end = run(dir.path(), json!("https://github.com/o/r/pull/7"), 120).await;
    assert_eq!(end.outcome, RunOutcome::Red);
    assert_eq!(
        end.result,
        json!({ "checks": [{ "name": "build", "link": link, "log": "one\ntwo" }] })
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ci_still_pending_past_the_limit_ends_failed() {
    let dir = tempfile::tempdir().unwrap();
    set(dir.path(), "checks", "t\tpending\t0\thttps://x/t\t\n");
    set(dir.path(), "code", "8");
    let end = run(dir.path(), json!(7), 0).await;
    let RunOutcome::Failed(why) = end.outcome else {
        panic!("{:?}", end.outcome)
    };
    assert!(why.contains("pending after the limit"), "{why}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_gh_error_is_retried_and_the_fifth_in_a_row_ends_failed() {
    let dir = tempfile::tempdir().unwrap();
    set(dir.path(), "error", "");
    let end = run(dir.path(), json!(7), 120).await;
    let RunOutcome::Failed(why) = end.outcome else {
        panic!("{:?}", end.outcome)
    };
    assert!(why.contains("boom"), "{why}");
    assert_eq!(calls(dir.path()).len(), 5);
}

#[tokio::test]
async fn a_piece_with_no_change_ends_failed_without_asking_gh() {
    let dir = tempfile::tempdir().unwrap();
    let end = run(dir.path(), Value::Null, 120).await;
    assert_eq!(
        end.outcome,
        RunOutcome::Failed("The piece has no change to watch.".into())
    );
    assert!(calls(dir.path()).is_empty());
}
