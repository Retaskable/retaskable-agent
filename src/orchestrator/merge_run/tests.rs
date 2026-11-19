//! `merge` runs against a fake store, a fake `gh` and temp git repos (a bare origin and a clone).
use std::path::Path;
use std::sync::Mutex;

use axum::{Json, Router, extract::State, routing::post};

use super::*;
use crate::backlog::merge::MergeOutcome;
use crate::backlog::runs::BeatVerdict;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::{RunEnd, RunEndSender};
use crate::orchestrator::watch_run::tests::lane_free_request;
use crate::test_support::{git as tgit, repo_with_origin, run as trun, write_executable};

type Log = Arc<Mutex<(Vec<Value>, bool)>>;

/// A store that answers every op with null (a `merge_beat` with `held`, unless the log's flag says the turn is lost),
/// recording each request.
async fn store(mark: PathBuf) -> (String, Log) {
    let log: Log = Arc::new(Mutex::new((Vec::new(), false)));
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                |State((s, mark)): State<(Log, PathBuf)>, Json(body): Json<Value>| async move {
                    let mut s = s.lock().unwrap();
                    let value = if body["op"] == "merge_beat" {
                        std::fs::write(&mark, "").unwrap();
                        json!({ "held": !s.1 })
                    } else if body["op"] == "merge_result" {
                        json!({ "held": true })
                    } else {
                        Value::Null
                    };
                    s.0.push(body);
                    Json(json!({ "value": value }))
                },
            ),
        )
        .with_state((log.clone(), mark));
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

/// Waits of a few milliseconds, with the unknown wait a little longer than a read.
fn quick() -> Delays {
    Delays {
        ask_every: Duration::from_millis(5),
        unknown_for: Some(Duration::from_millis(40)),
        retry_after: Some(Duration::from_millis(5)),
    }
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
  *mergeCommit*) echo '{"mergeCommit":{"oid":"abc1234567890abcdef1234567890abcdef12345"}}';;
  *statusCheckRollup*) cat "$d/checks" 2>/dev/null || echo '{"statusCheckRollup":[]}'; if [ -f "$d/checks.next" ]; then mv "$d/checks.next" "$d/checks"; fi;;
  "pr view "*) [ -f "$d/slow" ] && sleep 1; cat "$d/view"; if [ -f "$d/view.next" ]; then mv "$d/view.next" "$d/view"; fi;;
  "pr merge "*)
    [ -f "$d/beaten" ] && echo beaten >> "$d/after-beat"
    [ -f "$d/view-after-merge" ] && cp "$d/view-after-merge" "$d/view"
    # `fail-times` N: the first N merges are refused with `merge-err`; `advance`: base moves on first.
    if [ -f "$d/advance" ]; then
      rm "$d/advance"
      export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
      git push -q origin "$(git commit-tree -p origin/main -m adv 'origin/main^{tree}')":refs/heads/main
    fi
    if [ -f "$d/fail-times" ] && [ "$(cat "$d/fail-times")" -gt 0 ]; then
      echo $(( $(cat "$d/fail-times") - 1 )) > "$d/fail-times"
      cat "$d/merge-err" >&2
      exit 1
    fi
    [ -f "$d/merge-err" ] && [ ! -f "$d/fail-times" ] && cat "$d/merge-err" >&2
    exit $(cat "$d/merge-code" 2>/dev/null || echo 0);;
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

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo_with_feat(dir.path());
    set(dir.path(), "view", MERGEABLE);
    let (url, log) = store(dir.path().join("beaten")).await;
    let (ends, rx) = RunEndSender::channel();
    let config = ProjectConfig {
        repo_path: repo.display().to_string(),
        worktrees_dir: dir.path().join("wt").display().to_string(),
        base_branch: "main".into(),
        max_conflict_rounds: 5,
        ..Default::default()
    };
    let piece = serde_json::from_value(json!({ "id": "p1", "title": "t", "branch": "feat", "change": 7 })).unwrap();
    let request = lane_free_request("M1", RunKind::Merge, piece, config, &url, ends);
    // The loop records the turn it started the run with.
    request.context.locks.add_turn("M1");
    Rig {
        dir,
        repo,
        log,
        request,
        ends: rx,
    }
}

impl Rig {
    async fn run(&mut self) -> RunEnd {
        self.run_with(quick()).await
    }

    async fn run_with(&mut self, delays: Delays) -> RunEnd {
        let runner = MergeRuns::with(fake_gh(self.dir.path()), tgit, delays);
        runner.start(self.request.clone()).unwrap();
        let end = tokio::time::timeout(Duration::from_secs(20), self.ends.recv())
            .await
            .expect("the run ended")
            .unwrap();
        // The loop ends a merge turn with `merge_result`, whose outcomes and fields are `run_done`'s for a merge run
        // (backlogPieceOps.ts `endPieceRun`), except `questions`, which becomes `needs_person`, and `red`, which is `ci_failed` (the agent's own).
        if !matches!(end.outcome, RunOutcome::Questions | RunOutcome::Red) {
            crate::backlog::store_contract::check(RunKind::Merge, &end);
        }
        end
    }

    /// The turn is let go of here and no lock is taken or dropped: the loop reports `merge_result`.
    fn assert_turn_released(&self) {
        assert_eq!(self.request.context.locks.count(), 0);
        for op in ["lock_take", "lock_drop", "run_done", "run_beat"] {
            assert!(ops(&self.log, op).is_empty(), "{op}");
        }
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
    let mut rig = rig().await;
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(
        end.result,
        json!({ "commit": "abc1234567890abcdef1234567890abcdef12345" })
    );
    let calls = calls(rig.dir.path());
    assert!(calls.contains(&"pr merge 7 --squash".to_string()), "{calls:?}");
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_passed_change_only_behind_its_base_merges_as_it_is() {
    // As TS does (agent/src/merge.ts): a change that passed merges though the base moved on; it goes through CI again
    // only when the host refuses it as behind. Bringing it up to date first sent a busy queue round CI for hours.
    let mut rig = rig().await;
    commit(&rig.repo, "base.txt", "base");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    let before = rig.origin_head("feat");
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(rig.origin_head("feat"), before, "nothing pushed to the branch");
    assert!(calls(rig.dir.path()).contains(&"pr merge 7 --squash".to_string()));
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_conflicting_change_ends_conflict_with_the_instruction() {
    let mut rig = rig().await;
    trun(tgit(&rig.repo).args(["checkout", "-q", "main"]));
    commit(&rig.repo, "feat.txt", "main's");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    // The host sees the conflict.
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","state":"OPEN"}"#,
    );
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Conflict);
    let text = end.result["instruction"].as_str().unwrap();
    assert!(
        text.contains("Merge origin/main") && text.contains("- feat.txt"),
        "{text}"
    );
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_conflict_gh_sees_and_a_refusal_to_merge_both_update_the_change() {
    let mut rig = rig().await;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","state":"OPEN"}"#,
    );
    // Nothing to merge in, so the update is a no-op push: `updated`.
    assert_eq!(rig.run().await.outcome, RunOutcome::Updated);

    let mut rig = self::rig().await;
    // The base moves on while gh refuses: only then is the base merged in.
    set(rig.dir.path(), "merge-code", "1");
    set(rig.dir.path(), "advance", "");
    set(
        rig.dir.path(),
        "merge-err",
        "Head branch is not up to date with the base branch",
    );
    assert_eq!(rig.run().await.outcome, RunOutcome::Updated);
    assert_eq!(merges(&rig).len(), 1);
}

#[tokio::test]
async fn an_already_merged_change_ends_merged_and_a_closed_one_fails() {
    let mut rig = rig().await;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"MERGED"}"#,
    );
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert!(!calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));

    let mut rig = self::rig().await;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"CLOSED"}"#,
    );
    assert!(matches!(rig.run().await.outcome, RunOutcome::Failed(_)));
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_turn_lost_before_landing_ends_interrupted_without_merging() {
    let mut rig = rig().await;
    set(rig.dir.path(), "slow", "");
    let locks = rig.request.context.locks.clone();
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        locks.apply_beat(&[("M1".to_string(), BeatVerdict::Stop)].into());
    });
    let end = rig.run().await;
    stopper.await.unwrap();
    assert_eq!(end.outcome, RunOutcome::Interrupted);
    assert!(!calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));
    rig.assert_turn_released();
    assert_eq!(rig.request.context.locks.count(), 0);
}

fn start_runner(rig: &Rig) {
    let runner = MergeRuns::with(fake_gh(rig.dir.path()), tgit, quick());
    runner.start(rig.request.clone()).unwrap();
}

#[tokio::test]
async fn a_stop_between_processes_lets_the_turn_go_and_merges_nothing() {
    let mut rig = rig().await;
    set(rig.dir.path(), "slow", "");
    start_runner(&rig);
    rig.request.stop.set();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!calls(rig.dir.path()).iter().any(|c| c.starts_with("pr merge")));
    rig.assert_turn_released();
    assert_eq!(rig.request.context.locks.count(), 0);
    assert!(rig.ends.try_recv().is_err());
}

/// Makes the rig's piece a main sync on a project merging with `method`.
fn into_sync(rig: &mut Rig, method: &str) {
    rig.request.piece = serde_json::from_value(
        json!({ "id": "p1", "title": "t", "branch": "feat", "change": 7, "chore": { "kind": "sync_main" } }),
    )
    .unwrap();
    rig.request.context.config.github = Some(json!({ "mergeMethod": method }));
}

#[tokio::test]
async fn a_main_sync_merges_with_a_merge_commit_whatever_the_projects_method() {
    for method in ["squash", "rebase"] {
        let mut rig = rig().await;
        into_sync(&mut rig, method);
        let end = rig.run().await;
        assert_eq!(end.outcome, RunOutcome::Merged, "{method}");
        let calls = calls(rig.dir.path());
        assert!(calls.contains(&"pr merge 7 --merge".to_string()), "{calls:?}");
    }
}

#[tokio::test]
async fn a_refusal_of_merge_commits_ends_waiting_on_a_person() {
    let mut rig = rig().await;
    into_sync(&mut rig, "squash");
    set(rig.dir.path(), "merge-code", "1");
    set(
        rig.dir.path(),
        "merge-err",
        "Merge commits are not allowed on this repository.",
    );
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Questions);
    let q = end.result["questions"][0]["question"].as_str().unwrap();
    assert_eq!(q, "Allow merge commits in the repo's settings, or sync by hand");
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_conflicting_sync_is_told_to_merge_the_base_in_again_never_to_rebase() {
    let mut rig = rig().await;
    into_sync(&mut rig, "squash");
    commit(&rig.repo, "feat.txt", "main's");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    // The host sees the conflict.
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","state":"OPEN"}"#,
    );
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Conflict);
    let text = end.result["instruction"].as_str().unwrap();
    assert!(
        text.contains("Merge origin/main into your branch again") && text.contains("never a rebase"),
        "{text}"
    );
}

/// Sets the rig's project `github` settings.
fn github(rig: &mut Rig, settings: Value) {
    rig.request.context.config.github = Some(settings);
}

fn merges(rig: &Rig) -> Vec<String> {
    calls(rig.dir.path())
        .into_iter()
        .filter(|c| c.starts_with("pr merge"))
        .collect()
}

#[tokio::test]
async fn a_piece_where_a_person_merges_lands_once_with_the_projects_method() {
    for (method, flag) in [("squash", "--squash"), ("merge", "--merge"), ("rebase", "--rebase")] {
        let mut rig = rig().await;
        github(&mut rig, json!({ "land": "person", "mergeMethod": method }));
        let end = rig.run().await;
        assert_eq!(end.outcome, RunOutcome::Merged, "{method}");
        assert_eq!(merges(&rig), vec![format!("pr merge 7 {flag}")]);
        rig.assert_turn_released();
    }
}

#[tokio::test]
async fn the_branch_is_deleted_only_when_the_project_says_so() {
    for (land, delete, want) in [
        ("retaskable", false, "pr merge 7 --squash"),
        ("person", false, "pr merge 7 --squash"),
        ("retaskable", true, "pr merge 7 --squash"),
    ] {
        let mut rig = rig().await;
        github(&mut rig, json!({ "land": land, "deleteBranch": delete }));
        assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
        assert_eq!(merges(&rig), vec![want.to_string()], "{land} {delete}");
    }
}

#[tokio::test]
async fn landing_only_ever_runs_gh_pr_merge_and_never_pushes_to_the_base() {
    let mut rig = rig().await;
    github(&mut rig, json!({ "land": "person" }));
    let before = rig.origin_head("main");
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    assert_eq!(rig.origin_head("main"), before);
    assert!(
        calls(rig.dir.path()).iter().all(|c| c.starts_with("pr ")),
        "{:?}",
        calls(rig.dir.path())
    );
}

#[tokio::test]
async fn a_main_sync_is_never_a_persons_merge() {
    let mut rig = rig().await;
    into_sync(&mut rig, "squash");
    github(&mut rig, json!({ "land": "person" }));
    assert!(matches!(rig.run().await.outcome, RunOutcome::Failed(_)));
    assert!(calls(rig.dir.path()).is_empty());
}

#[tokio::test]
async fn a_roadmap_piece_squashes_and_deletes_by_default() {
    let mut rig = rig().await;
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig), vec!["pr merge 7 --squash".to_string()]);
}

#[tokio::test]
async fn the_turn_is_beaten_with_phase_merging_before_gh_pr_merge() {
    let mut rig = rig().await;
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    let beats = ops(&rig.log, "merge_beat");
    assert_eq!(beats.len(), 1);
    assert_eq!(beats[0]["args"], json!({ "token": "M1", "phase": "merging" }));
    // The fake gh saw the store's beat already made when `pr merge` ran.
    assert!(rig.dir.path().join("after-beat").exists());
    assert_eq!(merges(&rig).len(), 1);
}

#[tokio::test]
async fn a_turn_the_store_no_longer_holds_merges_nothing_and_ends_interrupted() {
    let mut rig = rig().await;
    rig.log.lock().unwrap().1 = true;
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Interrupted);
    assert_eq!(ops(&rig.log, "merge_beat").len(), 1);
    assert!(merges(&rig).is_empty());
    assert!(!rig.request.context.locks.held("M1"), "the lost turn's record goes");
}

#[tokio::test]
async fn a_store_that_cant_be_asked_for_the_beat_merges_nothing() {
    let mut rig = rig().await;
    rig.request.context.client = Arc::new(crate::backlog::Client::new("http://127.0.0.1:9".to_string()));
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Interrupted);
    assert!(merges(&rig).is_empty());
}

fn gh_says(rig: &Rig, why: &str, times: u32) {
    set(rig.dir.path(), "merge-err", why);
    set(rig.dir.path(), "fail-times", &times.to_string());
}

const NOT_READY: &str = "GraphQL: Base branch was modified. Review and try the merge again.";

#[tokio::test]
async fn a_merge_github_wasnt_ready_for_is_tried_again_and_the_turn_is_beaten_each_time() {
    let mut rig = rig().await;
    gh_says(&rig, NOT_READY, 2);
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig).len(), 3);
    assert_eq!(ops(&rig.log, "merge_beat").len(), 3);
}

#[tokio::test]
async fn retries_stop_at_max_merge_retries_and_then_a_person_is_needed() {
    for (cap, tries) in [(0, 1), (2, 3), (5, 6)] {
        let mut rig = rig().await;
        rig.request
            .context
            .config
            .tuning
            .store
            .as_mut()
            .unwrap()
            .max_merge_retries = cap;
        gh_says(&rig, NOT_READY, 99);
        let RunOutcome::Failed(why) = rig.run().await.outcome else {
            panic!("should fail")
        };
        assert!(why.contains("Base branch was modified"), "{why}");
        assert_eq!(merges(&rig).len(), tries, "cap {cap}");
        // A failed run is `needs_person` to the store.
        assert!(matches!(
            crate::backlog::merge::MergeOutcome::of_run(&RunOutcome::Failed(why), &json!({})),
            crate::backlog::merge::MergeOutcome::NeedsPerson { .. }
        ));
    }
}

#[tokio::test]
async fn a_refusal_that_isnt_not_ready_isnt_retried() {
    let mut rig = rig().await;
    gh_says(
        &rig,
        "GraphQL: Pull request is in clean status but blocked by review",
        99,
    );
    assert!(matches!(rig.run().await.outcome, RunOutcome::Failed(_)));
    assert_eq!(merges(&rig).len(), 1);
}

const UNKNOWN: &str = r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","state":"OPEN"}"#;

#[tokio::test]
async fn a_change_github_never_works_out_is_brought_up_to_date_after_the_wait() {
    let mut rig = rig().await;
    set(rig.dir.path(), "view", UNKNOWN);
    let started = std::time::Instant::now();
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Updated);
    // It waited the whole window before updating. (How many reads fit in it depends on how fast the fake gh starts,
    // which a loaded machine slows: only the wait itself is checked.)
    assert!(
        started.elapsed() >= quick().unknown_for.unwrap(),
        "updated before the wait was over"
    );
    assert!(calls(rig.dir.path()).iter().any(|c| c.starts_with("pr view")));
    assert!(merges(&rig).is_empty());
}

#[tokio::test]
async fn an_unknown_that_clears_within_the_wait_lands_as_usual() {
    let mut rig = rig().await;
    // The first read is UNKNOWN and the fake gh's next read is MERGEABLE (`view.next`): the change clears on a read, not
    // on a clock, and the wait is long enough that a slow machine can't run it out first.
    set(rig.dir.path(), "view", UNKNOWN);
    set(rig.dir.path(), "view.next", MERGEABLE);
    rig.request.context.config.base_branch = "main".into();
    let delays = Delays {
        unknown_for: Some(Duration::from_secs(60)),
        ..quick()
    };
    assert_eq!(rig.run_with(delays).await.outcome, RunOutcome::Merged);
}

#[tokio::test]
async fn with_no_conflict_rounds_left_an_unknown_or_a_conflict_needs_a_person_and_a_behind_merges_as_it_is() {
    let mut rig = rig().await;
    rig.request.context.config.max_conflict_rounds = 0;
    set(rig.dir.path(), "view", UNKNOWN);
    let RunOutcome::Failed(why) = rig.run().await.outcome else {
        panic!("unknown should fail")
    };
    assert!(why.contains("has not worked out"), "{why}");

    let mut rig = self::rig().await;
    rig.request.context.config.max_conflict_rounds = 0;
    set(
        rig.dir.path(),
        "view",
        r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","state":"OPEN"}"#,
    );
    let RunOutcome::Failed(why) = rig.run().await.outcome else {
        panic!("conflict should fail")
    };
    assert!(why.contains("maxConflictRounds 0"), "{why}");
    let before = rig.origin_head("feat");
    assert_eq!(rig.origin_head("feat"), before);

    // Only behind: nothing to update with, so it lands as it is.
    let mut rig = self::rig().await;
    rig.request.context.config.max_conflict_rounds = 0;
    commit(&rig.repo, "base.txt", "base");
    trun(tgit(&rig.repo).args(["push", "-q", "origin", "main"]));
    let before = rig.origin_head("feat");
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    assert_eq!(rig.origin_head("feat"), before);
}

#[tokio::test]
async fn a_main_sync_that_github_never_works_out_needs_a_person_not_an_update() {
    let mut rig = rig().await;
    into_sync(&mut rig, "squash");
    set(rig.dir.path(), "view", UNKNOWN);
    assert!(matches!(rig.run().await.outcome, RunOutcome::Failed(_)));
}

const LOCAL_DELETE_FAILED: &str =
    "failed to delete local branch feat: Cannot delete branch 'feat' checked out at '/wt/lane-2'";

#[tokio::test]
async fn a_merge_that_errors_after_landing_is_a_merge_when_the_change_shows_merged() {
    let mut rig = rig().await;
    set(rig.dir.path(), "merge-code", "1");
    set(rig.dir.path(), "merge-err", LOCAL_DELETE_FAILED);
    set(rig.dir.path(), "view-after-merge", r#"{"state":"MERGED"}"#);
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    rig.assert_turn_released();
}

#[tokio::test]
async fn a_merge_that_errors_and_leaves_the_change_open_still_fails() {
    let mut rig = rig().await;
    set(rig.dir.path(), "merge-code", "1");
    set(rig.dir.path(), "merge-err", LOCAL_DELETE_FAILED);
    assert!(matches!(rig.run().await.outcome, RunOutcome::Failed(_)));
}

mod auto;

const GREEN: &str = r#"{"statusCheckRollup":[{"__typename":"CheckRun","name":"ci/agent/pr/ci","status":"COMPLETED","conclusion":"SUCCESS"},{"__typename":"StatusContext","context":"lint","state":"SUCCESS"}]}"#;
const RED: &str = r#"{"statusCheckRollup":[{"__typename":"StatusContext","context":"ci/agent/pr/ci","state":"FAILURE"},{"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS"},{"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"TIMED_OUT"}]}"#;
const PENDING: &str =
    r#"{"statusCheckRollup":[{"__typename":"CheckRun","name":"build","status":"IN_PROGRESS","conclusion":""}]}"#;

#[tokio::test]
async fn a_head_whose_ci_failed_is_never_merged_and_ends_red_with_the_failing_checks() {
    let mut rig = rig().await;
    set(rig.dir.path(), "checks", RED);
    let end = rig.run().await;
    assert_eq!(end.outcome, RunOutcome::Red);
    assert_eq!(end.result["checks"], json!(["ci/agent/pr/ci", "lint"]));
    assert!(merges(&rig).is_empty());
    assert_eq!(
        MergeOutcome::of_run(&end.outcome, &end.result),
        MergeOutcome::CiFailed {
            checks: json!(["ci/agent/pr/ci", "lint"])
        }
    );
}

#[tokio::test]
async fn pending_checks_are_waited_for_and_a_green_head_then_merges() {
    let mut rig = rig().await;
    set(rig.dir.path(), "checks", PENDING);
    set(rig.dir.path(), "checks.next", GREEN);
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig).len(), 1);
}

#[tokio::test]
async fn checks_that_stay_pending_give_the_turn_back_unmerged() {
    let mut rig = rig().await;
    set(rig.dir.path(), "checks", PENDING);
    assert_eq!(rig.run().await.outcome, RunOutcome::Interrupted);
    assert!(merges(&rig).is_empty());
}

#[tokio::test]
async fn a_green_head_merges() {
    let mut rig = rig().await;
    set(rig.dir.path(), "checks", GREEN);
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig).len(), 1);
}

#[tokio::test]
async fn a_head_with_no_checks_at_all_merges_as_a_project_without_ci() {
    let mut rig = rig().await;
    set(rig.dir.path(), "checks", r#"{"statusCheckRollup":[]}"#);
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
    assert_eq!(merges(&rig).len(), 1);
}

#[tokio::test]
async fn a_head_with_no_checks_does_not_merge_when_the_project_requires_ci() {
    let mut rig = rig().await;
    rig.request.context.config.require_ci = true;
    rig.request.context.config.ci_when_ready = Some(true);
    set(rig.dir.path(), "checks", r#"{"statusCheckRollup":[]}"#);
    assert_eq!(rig.run().await.outcome, RunOutcome::Interrupted);
    assert!(merges(&rig).is_empty());
    assert!(
        calls(rig.dir.path()).iter().any(|c| c.contains("labels[]=ci-ready")),
        "the label is kept on while waiting"
    );
}

#[tokio::test]
async fn checks_that_show_up_late_on_a_project_requiring_ci_then_merge() {
    let mut rig = rig().await;
    rig.request.context.config.require_ci = true;
    set(rig.dir.path(), "checks", r#"{"statusCheckRollup":[]}"#);
    set(rig.dir.path(), "checks.next", GREEN);
    assert_eq!(rig.run().await.outcome, RunOutcome::Merged);
}
