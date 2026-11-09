use super::*;
use crate::setup_check::{SetupCheck, SetupCheckRunner, SetupChecks};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn stub(runs: Arc<AtomicUsize>, signed_in: Arc<AtomicBool>) -> Arc<SetupCheckRunner> {
    Arc::new(SetupCheckRunner::with_probe(Box::new(move || {
        runs.fetch_add(1, Ordering::SeqCst);
        let ok = signed_in.load(Ordering::SeqCst);
        Box::pin(async move {
            let c = || SetupCheck::unknown("x", None);
            SetupChecks {
                git: c(),
                gh_installed: c(),
                gh: c(),
                claude: c(),
                claude_signed_in: if ok { SetupCheck::pass("in") } else { c() },
                cursor: c(),
                pi: None,
                codex: None,
                docker: None,
            }
        })
    })))
}

fn go(v: &AgentView, project: &str, c: &Command, seen: &mut Seen) -> Value {
    run(c, project, seen, v, &|_| {}, &data_dir())
}

#[tokio::test]
async fn setup_check_is_pending_then_acks_with_the_checks_before_and_after_setup() {
    for project in ["", "one"] {
        let runs = Arc::new(AtomicUsize::new(0));
        let signed_in = Arc::new(AtomicBool::new(true));
        let mut v = view();
        v.checks = stub(runs.clone(), signed_in);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        v.item_commands.set_out(project, Some(tx));
        for args in [json!({}), Value::Null] {
            let mut c = cmd("setup_check", args);
            c.id = format!("c-{project}-{}", runs.load(Ordering::SeqCst));
            let answer = go(&v, project, &c, &mut Seen::default());
            assert_eq!(answer, json!({ "type": PENDING, "id": c.id }));
            let ack = rx.recv().await.unwrap();
            assert_eq!(ack["type"], "ack");
            assert_eq!(ack["id"], c.id);
            assert_eq!(ack["answer"], v.checks.latest().unwrap().to_json());
            assert_eq!(ack["answer"]["claudeSignedIn"]["ok"], true);
        }
    }
}

#[tokio::test]
async fn setup_check_with_an_argument_is_a_nack_and_runs_no_probe() {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut v = view();
    v.checks = stub(runs.clone(), Arc::default());
    for args in [json!({ "x": 1 }), json!([]), json!("x")] {
        let answer = go(&v, "", &cmd("setup_check", args), &mut Seen::default());
        assert_eq!(answer["reason"], "setup_check takes no arguments.");
    }
    tokio::task::yield_now().await;
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_replayed_or_expired_setup_check_is_refused_and_runs_no_probe() {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut v = view();
    v.checks = stub(runs.clone(), Arc::default());
    let mut seen = Seen::default();
    assert_eq!(go(&v, "", &cmd("setup_check", json!({})), &mut seen)["type"], PENDING);
    let again = go(&v, "", &cmd("setup_check", json!({})), &mut seen);
    assert!(again["reason"].as_str().unwrap().starts_with("Already received"));
    let mut old = cmd("setup_check", json!({}));
    old.id = "old".into();
    old.expires_at = now_ms() - 1;
    let late = go(&v, "", &old, &mut seen);
    assert!(late["reason"].as_str().unwrap().starts_with("Expired"));
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_changed_result_ticks_the_change_watch_and_an_equal_one_does_not() {
    let signed_in = Arc::new(AtomicBool::new(false));
    let runner = stub(Arc::default(), signed_in.clone());
    let mut rx = runner.subscribe();
    runner.run_now().await;
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();
    runner.run_now().await;
    assert!(!rx.has_changed().unwrap());
    signed_in.store(true, Ordering::SeqCst);
    runner.run_now().await;
    assert!(rx.has_changed().unwrap());
}

#[test]
fn the_audit_line_records_the_ack() {
    let tmp = tempfile::tempdir().unwrap();
    let c = cmd("setup_check", json!({}));
    audit(
        tmp.path(),
        &c,
        &json!({ "type": "ack", "id": "c1", "answer": { "git": {} } }),
        "url",
    );
    let line = std::fs::read_to_string(tmp.path().join("remote.log")).unwrap();
    let line: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(
        (line["name"].as_str(), line["outcome"].as_str()),
        (Some("setup_check"), Some("ack"))
    );
}
