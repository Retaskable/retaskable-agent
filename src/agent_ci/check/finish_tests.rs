//! A `ci_finish` the store can't take is kept and sent again, also after a restart in the middle of an outage (the
//! owner's outage rule, 2026-11-16).
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::super::store::tests::FakeStore;
use super::*;

/// A store that is unreachable for its first `down` calls, then answers.
fn down_for(down: usize) -> Arc<FakeStore> {
    let seen = AtomicUsize::new(0);
    FakeStore::answering(move |_, _| {
        if seen.fetch_add(1, Ordering::SeqCst) < down {
            Err("the store couldn't be reached".into())
        } else {
            Ok(json!({ "ok": true }))
        }
    })
}

fn args() -> serde_json::Value {
    json!({ "sha": "h1", "context": "agent-ci/pr", "event": "pull_request", "pr": 7, "result": "passed" })
}

fn files(dir: &std::path::Path) -> usize {
    Kept::all(dir).len()
}

#[tokio::test(start_paused = true)]
async fn three_quick_tries_then_the_record_is_kept_and_sent_when_the_store_is_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = down_for(5);
    let shared: Arc<dyn CiStore> = store.clone();
    let args = args();
    let kept = Kept::of(dir.path(), "o/p", &args);
    let record = kept.save("o/p", &args);
    assert!(!try_finish(&shared, "o/p", &args, Duration::from_secs(5)).await);
    assert_eq!(store.args("ci_finish").len(), 3);
    assert_eq!(files(dir.path()), 1, "on disk while the store can't take it");
    // Kept: sent again every half minute until the store takes it, then no more.
    keep_finishing(shared, kept, record, false).await;
    let sent = store.args("ci_finish");
    assert_eq!(sent.len(), 6, "3 quick, then 2 refused and 1 taken");
    assert!(sent.iter().all(|a| *a == args), "the same record each time");
    assert_eq!(files(dir.path()), 0, "off the disk once taken");
}

#[tokio::test(start_paused = true)]
async fn a_record_the_store_takes_at_once_is_sent_once() {
    let store = down_for(0);
    let shared: Arc<dyn CiStore> = store.clone();
    assert!(try_finish(&shared, "o/p", &json!({}), Duration::from_secs(5)).await);
    assert_eq!(store.args("ci_finish").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_restart_in_an_outage_sends_the_record_at_once_and_it_is_taken_once_the_store_is_back() {
    let dir = tempfile::tempdir().unwrap();
    // The old process listed it and died with the store out; its folder is all that is left.
    Kept::of(dir.path(), "o/p", &args()).save("o/p", &args());

    let (kept, record) = Kept::all(dir.path()).pop().unwrap();
    assert_eq!(record.args, args(), "read back as it was kept");
    let store = down_for(2);
    let shared: Arc<dyn CiStore> = store.clone();
    let task = tokio::spawn(keep_finishing(shared, kept, record, true));
    tokio::task::yield_now().await;
    assert_eq!(store.args("ci_finish").len(), 1, "sent at once");
    assert_eq!(files(dir.path()), 1, "still owed");
    task.await.unwrap();
    assert_eq!(store.args("ci_finish").len(), 3, "two refused, one taken, no more");
    assert_eq!(files(dir.path()), 0);
}

#[tokio::test(start_paused = true)]
async fn a_record_kept_for_a_day_is_tried_once_more_then_given_up_and_leaves_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let kept = Kept::of(dir.path(), "o/p", &args());
    let mut record = kept.save("o/p", &args());
    record.since -= 2 * 24 * 60 * 60;
    let store = down_for(usize::MAX);
    let shared: Arc<dyn CiStore> = store.clone();
    keep_finishing(shared, kept, record, true).await;
    assert_eq!(store.args("ci_finish").len(), 1, "already past its day: one try");
    assert_eq!(files(dir.path()), 0);
}

#[tokio::test(start_paused = true)]
async fn the_loop_resends_what_the_data_folder_holds_when_it_starts() {
    use crate::agent_ci::{CiLoop, Programs, settings::Repo, settings::Settings};
    let dir = tempfile::tempdir().unwrap();
    Kept::of(dir.path(), "o/p", &args()).save("o/p", &args());
    let cfg = Settings {
        repos: vec![Repo::parse("o/p").unwrap()],
        slots: 1,
        cpus: 2,
        computer: "t".into(),
        data_dir: dir.path().to_path_buf(),
    };
    let store = down_for(0);
    CiLoop::new(Programs::default())
        .with_store(store.clone())
        .resend_kept(&cfg);
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert_eq!(store.args("ci_finish"), [args()], "sent at once, once");
    assert_eq!(files(dir.path()), 0);
}
