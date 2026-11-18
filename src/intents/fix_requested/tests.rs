use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;

fn row(args: Value) -> IntentRow {
    serde_json::from_value(json!({ "id": "in-1", "kind": "fix_requested", "status": "requested", "args": args }))
        .unwrap()
}

fn good() -> IntentRow {
    row(json!({ "pr": 12, "sha": "abc1234" }))
}

/// The watcher, scripted: what it answers, and the presses it was asked.
struct Watcher {
    answer: Result<String, PressError>,
    asked: Mutex<Vec<(u64, String)>>,
}

impl FixPress for Watcher {
    fn press(&self, pr: u64, sha: &str) -> BoxFuture<'static, Result<String, PressError>> {
        self.asked.lock().unwrap().push((pr, sha.into()));
        let answer = self.answer.clone();
        async move { answer }.boxed()
    }
}

fn watcher(answer: Result<String, PressError>) -> (FixCx, Arc<Watcher>) {
    let w = Arc::new(Watcher {
        answer,
        asked: Mutex::default(),
    });
    (Some(w.clone()), w)
}

fn handler(runner: bool) -> FixHandler {
    FixHandler::new(Arc::new(AtomicBool::new(runner)))
}

#[test]
fn it_starts_work_and_runs_a_session_and_is_taken_only_by_the_runner() {
    let flag = Arc::new(AtomicBool::new(false));
    let h = FixHandler::new(flag.clone());
    assert_eq!(h.kind(), "fix_requested");
    assert!(h.starts_work() && h.runs_session());
    assert!(!h.not_twice());
    // Not the runner: left alone. The store may name this computer the runner later.
    assert!(!h.takes(&good()));
    flag.store(true, Ordering::SeqCst);
    assert!(h.takes(&good()));
}

#[tokio::test]
async fn a_press_that_goes_ahead_ends_the_row_done_with_what_it_says() {
    let (cx, w) = watcher(Ok("Trying a fix for pull request #12.".into()));
    let outcome = handler(true).run(good(), &cx).await;
    assert_eq!(
        outcome,
        Outcome::Done(json!({ "says": "Trying a fix for pull request #12." }))
    );
    assert_eq!(*w.asked.lock().unwrap(), [(12, "abc1234".to_string())]);
}

#[tokio::test]
async fn a_refusal_is_refused_in_its_words_and_a_breakage_is_failed() {
    let (cx, _) = watcher(Err(PressError::Refused(
        "A fix is already running for pull request #12.".into(),
    )));
    assert_eq!(
        handler(true).run(good(), &cx).await,
        Outcome::Refused("A fix is already running for pull request #12.".into())
    );
    let (cx, _) = watcher(Err(PressError::Failed("boom".into())));
    assert_eq!(handler(true).run(good(), &cx).await, Outcome::Failed("boom".into()));
}

#[tokio::test]
async fn no_watcher_is_refused_in_the_ts_words() {
    assert_eq!(
        handler(true).run(good(), &None).await,
        Outcome::Refused("This computer isn't watching pull requests.".into())
    );
}

#[tokio::test]
async fn arguments_that_are_not_a_pull_request_and_a_commit_are_refused_unasked() {
    let (cx, w) = watcher(Ok("x".into()));
    for args in [
        json!({}),
        json!({ "pr": 0, "sha": "abc1234" }),
        json!({ "pr": "12", "sha": "abc1234" }),
        json!({ "pr": 12 }),
        json!({ "pr": 12, "sha": "abc12" }),
        json!({ "pr": 12, "sha": "xyz1234" }),
        json!({ "pr": 12, "sha": "a".repeat(65) }),
    ] {
        let outcome = handler(true).run(row(args.clone()), &cx).await;
        assert!(matches!(outcome, Outcome::Refused(_)), "{args}: {outcome:?}");
    }
    assert!(w.asked.lock().unwrap().is_empty());
    // The longest commit is fine.
    let ok = handler(true)
        .run(row(json!({ "pr": 1, "sha": "A".repeat(64) })), &cx)
        .await;
    assert!(matches!(ok, Outcome::Done(_)));
}
