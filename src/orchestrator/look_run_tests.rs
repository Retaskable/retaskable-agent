use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use super::super::fake_session_tests::{assert_ran_read_only_in, cut_registered_tree, roster};
use super::*;
use crate::config::{Engine, EngineChoiceConfig};
use crate::env::Env;
use crate::orchestrator::driver::{RunEnd, RunEndSender, RunProcesses};

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    starter: LookRuns,
}

fn rig(out: &str, extra: &str, choice: EngineChoiceConfig) -> Rig {
    let (dir, root, roster) = roster(out, extra, choice, |p| p.budget.reviewer_usd = 1.5);
    Rig {
        _dir: dir,
        root,
        starter: LookRuns::new(roster, Slot::default()),
    }
}

fn answer(structured: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured},"session_id":"s"}}"#
    )
}

const AGAIN: &str =
    r#"{"decision":"again","reason":"tests were red","instructions":"run the formatter first","engine":"claude"}"#;

struct Started {
    ends: UnboundedReceiver<RunEnd>,
    trees: UnboundedReceiver<(String, Arc<ProcessTree>)>,
    stop: crate::runs::RunStop,
}

fn request_for(project: &str, piece: Value, other: Value) -> (RunRequest, Started) {
    let (ends, rx) = RunEndSender::channel();
    let (processes, trees) = RunProcesses::channel();
    let stop = crate::runs::RunStop::default();
    let request = RunRequest {
        token: "tok".into(),
        project: project.into(),
        processes,
        stop: stop.clone(),
        kind: RunKind::Look,
        piece: serde_json::from_value(piece).unwrap(),
        other: other.as_object().cloned().unwrap_or_default(),
        lane: None,
        ends,
        context: crate::orchestrator::driver::RunContext::for_test(),
    };
    (request, Started { ends: rx, trees, stop })
}

fn request(piece: Value) -> (RunRequest, Started) {
    request_for("p", piece, json!({}))
}

fn brief() -> Value {
    json!({
        "id": "pc1", "title": "Show the thing", "what": "Show it.", "doneWhen": "It shows.",
        "attempts": [{ "kind": "build", "outcome": "failed", "why": "tests red", "computer": "box-1" }]
    })
}

async fn end_of(rx: &mut UnboundedReceiver<RunEnd>) -> RunEnd {
    tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .expect("an end")
        .expect("the channel stays open")
}

async fn until_free(r: &Rig) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while r.starter.kinds().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_decision_ends_looked_with_its_wire_json_in_the_base_checkout() {
    let r = rig(&answer(AGAIN), "", EngineChoiceConfig::default());
    let base = lanes::base_checkout_path(&PathBuf::from(&r.starter.roster.projects[0].worktrees_dir));
    std::fs::create_dir_all(&base).unwrap();
    let (req, mut s) = request(brief());
    assert_eq!(r.starter.kinds(), vec![RunKind::Look]);
    // The settings page's status line lists the kind by this name.
    assert_eq!(serde_json::to_value(RunKind::Look).unwrap(), "look");
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.token, "tok");
    assert_eq!(end.outcome, RunOutcome::Looked, "{:?}", end.result);
    assert_eq!(
        end.result,
        json!({
            "decision": "again", "reason": "tests were red",
            "instructions": "run the formatter first", "engine": "claude"
        })
    );
    // It runs in the base checkout, read-only, on the middle tier with the reviewer's budget.
    assert_ran_read_only_in(&r.root, &base);
    // The prompt carries the piece and its attempts.
    let prompt = std::fs::read_to_string(r.root.join("in.txt")).unwrap();
    assert!(
        prompt.contains("Show the thing") && prompt.contains("tests red"),
        "{prompt}"
    );
    // The transcript goes under the run folder keyed by the piece.
    let files = std::fs::read_dir(paths::run_dir(&r.root.join("data"), "p", "pc1")).unwrap();
    assert_eq!(files.count(), 1);
}

#[tokio::test]
async fn a_plans_history_reaches_the_prompt() {
    let r = rig(&answer(AGAIN), "", EngineChoiceConfig::default());
    let (req, mut s) = request_for(
        "p",
        brief(),
        json!({ "history": ["planned it badly"], "knownBugs": [{ "id": "b1", "title": "Known bug" }] }),
    );
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::Looked);
    let prompt = std::fs::read_to_string(r.root.join("in.txt")).unwrap();
    assert!(
        prompt.contains("planned it badly") && prompt.contains("Known bug"),
        "{prompt}"
    );
}

#[tokio::test]
async fn without_a_base_checkout_it_runs_in_the_repo() {
    let r = rig(&answer(AGAIN), "", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::Looked);
    let pwd = std::fs::read_to_string(r.root.join("pwd.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(r.root.join("repo")).unwrap()
    );
}

#[tokio::test]
async fn an_unusable_answer_ends_failed_with_the_note() {
    for out in [
        answer(r#"{"decision":"again","reason":"r"}"#),
        answer(r#"{"decision":"maybe","reason":"r"}"#),
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s"}"#.to_string(),
    ] {
        let r = rig(&out, "", EngineChoiceConfig::default());
        let (req, mut s) = request(brief());
        r.starter.start(req).unwrap();
        let end = end_of(&mut s.ends).await;
        let RunOutcome::Failed(note) = &end.outcome else {
            panic!("{:?}", end.outcome)
        };
        assert!(!note.is_empty(), "{note}");
        assert_eq!(end.result, json!({}));
    }
}

#[tokio::test]
async fn a_session_that_fails_ends_failed() {
    let r = rig("", "exit 3", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert!(matches!(end_of(&mut s.ends).await.outcome, RunOutcome::Failed(_)));
    until_free(&r).await;
}

#[tokio::test]
async fn another_engine_or_a_missing_binary_gives_the_run_back() {
    let other = EngineChoiceConfig {
        default: Some(Engine::Codex),
        ..Default::default()
    };
    let r = rig(&answer(AGAIN), "", other);
    assert!(r.starter.kinds().is_empty());
    let (req, _s) = request(brief());
    assert!(r.starter.start(req).unwrap_err().contains("only Claude"));

    let mut r = rig(&answer(AGAIN), "", EngineChoiceConfig::default());
    let gone = r.root.join("nothing-here").to_string_lossy().into_owned();
    r.starter.roster.env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| gone.clone()));
    let (req, _s) = request(brief());
    assert!(r.starter.start(req).unwrap_err().contains("wasn't found"));
    // A run given back leaves the slot free.
    assert_eq!(r.starter.kinds(), vec![RunKind::Look]);

    let (req, _s) = request_for("other", brief(), json!({}));
    assert!(r.starter.start(req).unwrap_err().contains("no project other"));
}

#[tokio::test]
async fn one_look_at_a_time_and_its_tree_is_registered() {
    let r = rig(&answer(AGAIN), "sleep 60", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert!(r.starter.kinds().is_empty());
    let (second, _s2) = request(brief());
    assert!(r.starter.start(second).unwrap_err().contains("already running"));
    // Cutting the registered tree ends the run with nothing from the task, and frees the slot.
    cut_registered_tree(&mut s.trees).await;
    until_free(&r).await;
    assert_eq!(r.starter.kinds(), vec![RunKind::Look]);
    assert!(s.ends.try_recv().is_err());
}

#[tokio::test]
async fn a_stopped_run_sends_no_end() {
    let r = rig(&answer(AGAIN), "sleep 1", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    s.stop.set();
    until_free(&r).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(s.ends.try_recv().is_err());
}

#[test]
fn the_slot_is_held_until_its_guard_drops() {
    let slot = Slot::default();
    let shared = slot.clone();
    let guard = slot.take().unwrap();
    assert!(shared.busy() && shared.take().is_none());
    drop(guard);
    assert!(!shared.busy() && shared.take().is_some());
}
