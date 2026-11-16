//! A `ci_finish` the store can't take is kept and sent again (the owner's outage rule, 2026-11-16).
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

#[tokio::test(start_paused = true)]
async fn three_quick_tries_then_the_record_is_kept_and_sent_when_the_store_is_back() {
    let store = down_for(5);
    let shared: Arc<dyn CiStore> = store.clone();
    let args = json!({ "sha": "h1", "result": "passed" });
    assert!(!try_finish(&shared, "o/p", &args, Duration::from_secs(5)).await);
    assert_eq!(store.args("ci_finish").len(), 3);
    // Kept: sent again every half minute until the store takes it, then no more.
    keep_finishing(shared, "o/p".into(), args.clone()).await;
    let sent = store.args("ci_finish");
    assert_eq!(sent.len(), 6, "3 quick, then 2 refused and 1 taken");
    assert!(sent.iter().all(|a| *a == args), "the same record each time");
}

#[tokio::test(start_paused = true)]
async fn a_record_the_store_takes_at_once_is_sent_once() {
    let store = down_for(0);
    let shared: Arc<dyn CiStore> = store.clone();
    assert!(try_finish(&shared, "o/p", &json!({}), Duration::from_secs(5)).await);
    assert_eq!(store.args("ci_finish").len(), 1);
}
