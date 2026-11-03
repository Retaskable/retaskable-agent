//! `merge` runs against a fake store, a fake `gh` and temp git repos (a bare origin and a clone).
use std::path::Path;
use std::sync::Mutex;

use axum::{Json, Router, extract::State, routing::post};

use super::*;
use crate::backlog::runs::BeatVerdict;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::RunEndSender;
use crate::orchestrator::watch_run::tests::lane_free_request;
use crate::test_support::{git as tgit, repo_with_origin, run as trun, write_executable};

type Log = Arc<Mutex<(Vec<Value>, bool)>>;

/// A store whose `lock_take` gives token `L1`, or says `other-pc` holds it when the flag is set.
async fn store(held_by_other: bool) -> (String, Log) {
    let log: Log = Arc::new(Mutex::new((Vec::new(), held_by_other)));
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(|State(s): State<Log>, Json(body): Json<Value>| async move {
                let mut s = s.lock().unwrap();
                s.0.push(body.clone());
                let value = match body["op"].as_str().unwrap() {
                    "lock_take" if s.1 => json!({ "held": false, "by": "other-pc", "note": "piece-x" }),
                    "lock_take" => json!({ "held": true, "token": "L1" }),
                    _ => Value::Null,
                };
                Json(json!({ "value": value }))
            }),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), log)
}

fn ops(log: &Log, op: &str) -> Vec<Value> {
    log.lock()
        .unwrap()
        .0
        .iter()
        .filter(|b| b["op"] == op)
        .cloned()
        .collect()
}

/// A fake `gh`: `pr view … mergeCommit` answers the commit, any other `pr view` prints `view` (or sleeps first when
/// `slow` exists), `pr merge` exits `merge-code` (default 0, printing `merge-err`); every call is logged.
fn fake_gh(dir: &Path) -> PathBuf {
    let path = dir.join("fake_gh.sh");
    write_executable(
        &path,
        r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/calls.log"
case "$*" in
  *mergeCommit*) echo '{"mergeCommit":{"oid":"abc123"}}';;
  "pr view "*) [ -f "$d/slow" ] && sleep 1; cat "$d/view";;
  "pr merge "*) [ -f "$d/merge-err" ] && cat "$d/merge-err" >&2; exit $(cat "$d/merge-code" 2>/dev/null || echo 0);;
esac
"#,
    );
    path
}

fn set(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

fn calls(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default();
    text.lines().filter(|l| !l.is_empty()).map(String::from).collect()
}

const MERGEABLE: &str = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","state":"OPEN"}"#;

fn commit(repo: &Path, file: &str, text: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    trun(tgit(repo).args(["add", "."]));
    trun(tgit(repo).args(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]));
}

/// A repo whose `feat` branch is pushed from `main`'s head.
fn repo_with_feat(dir: &Path) -> PathBuf {
    let repo = repo_with_origin(dir);
    trun(tgit(&repo).args(["checkout", "-q", "-b", "feat"]));
    commit(&repo, "feat.txt", "feat");
    trun(tgit(&repo).args(["push", "-q", "origin", "feat"]));
    trun(tgit(&repo).args(["checkout", "-q", "main"]));
    repo
}

struct Rig {
    dir: tempfile::TempDir,
    repo: PathBuf,
    log: Log,
    request: RunRequest,
    ends: tokio::sync::mpsc::UnboundedReceiver<RunEnd>,
}

async fn rig(held_by_other: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_with_feat(dir.path());
    set(dir.path(), "view", MERGEABLE);
    let (url, log) = store(held_by_other).await;
    let (ends, rx) = RunEndSender::channel();
    let config = ProjectConfig {
        repo_path: repo.display().to_string(),
        worktrees_dir: dir.path().join("wt").display().to_string(),
        base_branch: "main".into(),
        ..Default::default()
    };
    let piece = serde_json::from_value(json!({ "id": "p1", "title": "t", "branch": "feat", "change": 7 })).unwrap();
    let request = lane_free_request("M1", RunKind::Merge, piece, config, &url, ends);
    Rig {
        dir,
        repo,
        log,
        request,
        ends: rx,
    }
}

impl Rig {
    async fn run(&mut self, wait: Option<Duration>) -> RunEnd {
        let runner = MergeRuns::with(
            fake_gh(self.dir.path()),
            tgit,
            "here".into(),
            Duration::from_millis(10),
            wait,
        );
        runner.start(self.request.clone()).unwrap();
        tokio::time::timeout(Duration::from_secs(20), self.ends.recv())
            .await
            .expect("the run ended")
            .unwrap()
    }

    fn assert_lock_dropped(&self) {
        let drops = ops(&self.log, "lock_drop");
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0]["args"]["token"], "L1");
    }

    fn origin_head(&self, branch: &str) -> String {
        let out = tgit(&self.repo).args(["ls-remote", "origin", branch]).output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    }
}

#[tokio::test]
async fn a_mergeable_up_to_date_change_is_squash_merged_and_ends_merged_with_the_commit() {
    let mut rig = rig(false).await;
    let end = rig.run(None).await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(end.result, json!({ "commit": "abc123" }));
    let calls = calls(rig.dir.path());
    assert!(
        calls.contains(&"pr merge 7 --squash --delete-branch".to_string()),
        "{calls:?}"
    );
    let take = &ops(&rig.log, "lock_take")[0]["args"];
    assert_eq!(take["name"], "merge-main");
    assert_eq!(take["note"], "p1 on here");
    rig.assert_lock_dropped();
}

#[tokio::test]
async fn a_behind_change_is_updated_and_pushed_without_asking_gh() {
    let mut rig = rig(false).await;
    commit(&rig.repo, "base.txt", "base");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    let before = rig.origin_head("feat");
    let end = rig.run(None).await;
    assert_eq!(end.outcome, RunOutcome::Updated);
    let after = rig.origin_head("feat");
    assert_ne!(before, after);
    assert_eq!(end.result["head"], after);
    assert!(calls(rig.dir.path()).is_empty());
    rig.assert_lock_dropped();
}

#[tokio::test]
async fn a_conflicting_change_ends_conflict_with_the_instruction() {
    let mut rig = rig(false).await;
    trun(tgit(&rig.repo).args(["checkout", "-q", "main"]));
    commit(&rig.repo, "feat.txt", "main's");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    let end = rig.run(None).await;
    assert_eq!(end.outcome, RunOutcome::Conflict);
    let text = end.result["instruction"].as_str().unwrap();
    assert!(
        text.contains("Merge origin/main") && text.contains("- feat.txt"),
        "{text}"
    );
    rig.assert_lock_dropped();
}

#[tokio::test]
async fn a_conflict_gh_sees_and_a_refusal_to_merge_both_update_the_change() {
    let mut rig = rig(false).await;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","state":"OPEN"}"#,
    );
    // Nothing to merge in, so the update is a no-op push: `updated`.
    assert_eq!(rig.run(None).await.outcome, RunOutcome::Updated);

    let mut rig = self::rig(false).await;
    set(rig.dir.path(), "merge-code", "1");
    set(
        rig.dir.path(),
        "merge-err",
        "Head branch is not up to date with the base branch",
    );
    assert_eq!(rig.run(None).await.outcome, RunOutcome::Updated);
    assert!(calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));
}

#[tokio::test]
async fn an_already_merged_change_ends_merged_and_a_closed_one_fails() {
    let mut rig = rig(false).await;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"MERGED"}"#,
    );
    let end = rig.run(None).await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert!(!calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));

    let mut rig = self::rig(false).await;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"CLOSED"}"#,
    );
    assert!(matches!(rig.run(None).await.outcome, RunOutcome::Failed(_)));
    rig.assert_lock_dropped();
}

#[tokio::test]
async fn a_lock_held_by_another_computer_is_asked_for_again_then_fails_naming_who_holds_it() {
    let mut rig = rig(true).await;
    let end = rig.run(Some(Duration::from_millis(100))).await;
    let RunOutcome::Failed(why) = end.outcome else {
        panic!("{:?}", end.outcome)
    };
    assert!(why.contains("other-pc") && why.contains("piece-x"), "{why}");
    assert!(ops(&rig.log, "lock_take").len() >= 2);
    assert!(calls(rig.dir.path()).is_empty());
    assert!(ops(&rig.log, "lock_drop").is_empty(), "there was no lock to drop");
}

#[tokio::test]
async fn a_lock_lost_before_landing_ends_interrupted_without_merging() {
    let mut rig = rig(false).await;
    set(rig.dir.path(), "slow", "");
    let locks = rig.request.context.locks.clone();
    let stopper = tokio::spawn(async move {
        loop {
            if let Some(token) = locks.tokens().first() {
                tokio::time::sleep(Duration::from_millis(300)).await;
                locks.apply_beat(&[(token.clone(), BeatVerdict::Stop)].into());
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    let end = rig.run(None).await;
    stopper.await.unwrap();
    assert_eq!(end.outcome, RunOutcome::Interrupted);
    assert!(!calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));
    rig.assert_lock_dropped();
    assert_eq!(rig.request.context.locks.count(), 0);
}

fn start_runner(rig: &Rig) {
    let runner = MergeRuns::with(
        fake_gh(rig.dir.path()),
        tgit,
        "here".into(),
        Duration::from_millis(10),
        None,
    );
    runner.start(rig.request.clone()).unwrap();
}

#[tokio::test]
async fn a_stop_while_waiting_for_the_lock_takes_nothing_and_merges_nothing() {
    let mut rig = rig(true).await;
    start_runner(&rig);
    while ops(&rig.log, "lock_take").len() < 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The loop ended the run (no process of it was running), and then the other computer let the lock go.
    rig.request.stop.set();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let asked = ops(&rig.log, "lock_take").len();
    rig.log.lock().unwrap().1 = false;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ops(&rig.log, "lock_take").len() <= asked + 1);
    assert_eq!(rig.request.context.locks.count(), 0, "no lock was taken");
    assert!(calls(rig.dir.path()).is_empty(), "no gh ran");
    assert!(rig.ends.try_recv().is_err(), "the loop already ended the run");
}

#[tokio::test]
async fn a_stop_between_processes_drops_the_lock_and_merges_nothing() {
    let mut rig = rig(false).await;
    set(rig.dir.path(), "slow", "");
    start_runner(&rig);
    while rig.request.context.locks.count() == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    rig.request.stop.set();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));
    rig.assert_lock_dropped();
    assert_eq!(rig.request.context.locks.count(), 0);
    assert!(rig.ends.try_recv().is_err());
}
