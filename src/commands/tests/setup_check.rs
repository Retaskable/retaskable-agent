use std::path::PathBuf;

use super::super::*;
use super::{cmd, data_dir, view};

/// A view whose runner's probe counts its runs and whose `setup-check-command` flag is `on`.
fn checks_view(runs: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> AgentView {
    use crate::setup_check::{SetupCheck, SetupCheckRunner, SetupChecks};
    let probe: crate::setup_check::runner::Probe = Box::new(move || {
        let n = runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            let c = || SetupCheck::unknown("x", None);
            SetupChecks {
                git: if n == 0 { SetupCheck::pass("first") } else { c() },
                gh_installed: c(),
                gh: c(),
                claude: c(),
                claude_signed_in: c(),
                cursor: c(),
                pi: None,
                codex: None,
                docker: None,
            }
        })
    });
    let mut v = view();
    v.checks = std::sync::Arc::new(SetupCheckRunner::with_probe(probe));
    v.settings = crate::hello_settings::HelloSettings::new(PathBuf::new(), crate::env::Env::empty(), 0);
    v
}

async fn ask_setup_check(v: &AgentView, project: &str, args: Value, seen: &mut Seen) -> Value {
    run(&cmd("setup_check", args), project, seen, v, &|_: i32| {}, &data_dir())
}

#[tokio::test]
async fn setup_check_is_pending_then_acks_with_the_checks_even_before_setup() {
    for project in ["one", ""] {
        let runs = Default::default();
        let v = checks_view(std::sync::Arc::clone(&runs));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        v.item_commands.set_out(project, Some(tx));
        for args in [json!({}), Value::Null] {
            let answer = ask_setup_check(&v, project, args, &mut Seen::default()).await;
            assert_eq!(answer, json!({ "type": PENDING, "id": "c1" }));
            let ack = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(ack["type"], "ack");
            assert_eq!(ack["id"], "c1");
            assert_eq!(ack["answer"], v.checks.latest().unwrap().to_json());
        }
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn setup_check_with_any_argument_is_a_nack_and_runs_nothing() {
    let runs: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let v = checks_view(runs.clone());
    let answer = ask_setup_check(&v, "one", json!({ "x": 1 }), &mut Seen::default()).await;
    assert_eq!(answer["type"], "nack");
    assert_eq!(answer["reason"], "setup_check takes no arguments.");
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn setup_check_is_refused_when_replayed_or_expired_() {
    let runs: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let v = checks_view(runs.clone());
    let mut seen = Seen::default();
    assert_eq!(ask_setup_check(&v, "one", json!({}), &mut seen).await["type"], PENDING);
    let replay = ask_setup_check(&v, "one", json!({}), &mut seen).await;
    assert_eq!(replay["type"], "nack");
    let mut old = cmd("setup_check", json!({}));
    old.expires_at = now_ms() - 1;
    let expired = run(&old, "one", &mut Seen::default(), &v, &|_: i32| {}, &data_dir());
    assert_eq!(expired["type"], "nack");
}

#[tokio::test]
async fn setup_check_ticks_the_change_watch_only_for_a_different_result() {
    let runs: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let v = checks_view(runs);
    let mut watch = v.checks.subscribe();
    v.checks.run_now().await; // first result counts
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();
    v.checks.run_now().await; // differs (the stub's second answer)
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();
    v.checks.run_now().await; // equal
    assert!(!watch.has_changed().unwrap());
}
