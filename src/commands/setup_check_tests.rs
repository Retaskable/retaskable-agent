use serde_json::{Value, json};

use super::tests::{cmd, data_dir, view};
use super::*;

fn stub_checks(detail: &str) -> crate::setup_check::SetupChecks {
    crate::setup_check::SetupChecks {
        git: crate::setup_check::SetupCheck::pass(detail),
        ..crate::setup_check::SetupChecks::all_unknown()
    }
}

/// A view whose runner's probe answers `details` in turn and counts its runs, with an open connection for `project`.
fn checks_view(
    details: Vec<&'static str>,
    project: &str,
) -> (
    AgentView,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    tokio::sync::mpsc::UnboundedReceiver<Value>,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runs = std::sync::Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let probe: crate::setup_check::runner::Probe = Box::new(move || {
        let n = r.fetch_add(1, Ordering::SeqCst);
        let checks = stub_checks(details[n.min(details.len() - 1)]);
        Box::pin(async move { checks })
    });
    let v = AgentView {
        checks: std::sync::Arc::new(crate::setup_check::SetupCheckRunner::with_probe(probe)),
        ..view()
    };
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    v.item_commands.set_out(project, Some(tx));
    (v, runs, rx)
}

#[tokio::test]
async fn setup_check_is_pending_then_acks_with_the_checks_even_before_setup() {
    for project in ["one", ""] {
        let (v, runs, mut rx) = checks_view(vec!["git 1"], project);
        let noop = |_: i32| {};
        for args in [json!({}), Value::Null] {
            let answer = run(
                &cmd("setup_check", args),
                project,
                &mut Seen::default(),
                &v,
                &noop,
                &data_dir(),
            );
            assert_eq!(answer, json!({ "type": PENDING, "id": "c1" }));
            let sent = rx.recv().await.unwrap();
            assert_eq!(
                sent,
                json!({ "type": "ack", "id": "c1", "answer": stub_checks("git 1").to_json() })
            );
        }
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn setup_check_with_any_argument_is_a_nack_and_runs_no_probe() {
    let (v, runs, _rx) = checks_view(vec!["git 1"], "one");
    let noop = |_: i32| {};
    let answer = run(
        &cmd("setup_check", json!({ "x": 1 })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["type"], "nack");
    assert_eq!(answer["reason"], "setup_check takes no arguments.");
    tokio::task::yield_now().await;
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn setup_check_replayed_or_expired_is_refused_without_a_probe() {
    let (v, runs, mut rx) = checks_view(vec!["git 1"], "one");
    let noop = |_: i32| {};
    let mut seen = Seen::default();
    let first = run(&cmd("setup_check", json!({})), "one", &mut seen, &v, &noop, &data_dir());
    assert_eq!(first["type"], PENDING);
    rx.recv().await.unwrap();
    let again = run(&cmd("setup_check", json!({})), "one", &mut seen, &v, &noop, &data_dir());
    assert_eq!(again["type"], "nack");
    let mut old = cmd("setup_check", json!({}));
    old.id = "c2".into();
    old.expires_at = now_ms() - 1;
    let expired = run(&old, "one", &mut Seen::default(), &v, &noop, &data_dir());
    assert_eq!(expired["type"], "nack");
    tokio::task::yield_now().await;
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn setup_check_ticks_the_change_watch_only_for_a_different_result() {
    let (v, _runs, mut rx) = checks_view(vec!["git 1", "git 1", "git 2"], "one");
    let noop = |_: i32| {};
    let mut watch = v.checks.subscribe();
    for (n, changed) in [true, false, true].into_iter().enumerate() {
        let mut c = cmd("setup_check", json!({}));
        c.id = format!("c{n}");
        run(&c, "one", &mut Seen::default(), &v, &noop, &data_dir());
        rx.recv().await.unwrap();
        assert_eq!(watch.has_changed().unwrap(), changed, "run {n}");
        watch.borrow_and_update();
    }
}
