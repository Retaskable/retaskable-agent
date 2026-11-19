//! docs/specs/agent-ci.md section 3.4 (2026-11-19): a rerun is a request the store records. A head the runner has finished (its status says
//! so, and it is still listed) is run again when the store asks, once per request; the store's claim decides whether there is a rerun.
use super::*;

fn ask(at: i64) -> Vec<serde_json::Value> {
    vec![json!({ "pr": 7, "sha": "h7", "at": at })]
}

/// One poll and the start of the pull request's key as the poll lists it (the way the loop does; the push to main is left out).
async fn poll_and_start(r: &mut Rig) {
    r.cfg.computer = "a".into();
    let mut keys = r.ci.poll(&r.cfg).await;
    keys.get_mut("o-p").unwrap().retain(|k| !k.is_push());
    for t in r.ci.start(&r.cfg, keys).await {
        t.await.unwrap();
    }
}

fn finished(r: &mut Rig) {
    let desc = format!("agent-ci z: done ({KEY_TEXT})");
    claim_gh(r, &[("failure", &desc, now() - 60_000)]);
}

#[tokio::test]
async fn a_finished_head_runs_again_for_a_rerun_the_store_asked_for_once_per_request() {
    let mut r = pr_rig("exit 0");
    finished(&mut r);
    // Not asked: a finished head is left alone (the status says so; no claim is made).
    poll_and_start(&mut r).await;
    assert_eq!((docker_runs(&r), r.store.args("ci_claim").len()), (0, 0));
    // Asked: it is claimed and run, whatever its status says.
    r.store.ask_reruns(ask(10));
    poll_and_start(&mut r).await;
    assert_eq!(
        (docker_runs(&r), r.store.args("ci_claim").len()),
        (1, 1),
        "{}",
        calls(&r)
    );
    // The same request again (the store has not seen the result yet): not run twice.
    poll_and_start(&mut r).await;
    assert_eq!(docker_runs(&r), 1);
    // A newer request is a new run.
    r.store.ask_reruns(ask(20));
    poll_and_start(&mut r).await;
    assert_eq!(docker_runs(&r), 2, "{}", calls(&r));
}

#[tokio::test]
async fn a_rerun_another_computer_goes_first_on_is_looked_at_again_at_the_next_poll() {
    let mut r = pr_rig("exit 0");
    finished(&mut r);
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let store = FakeStore::answering(move |op, _| {
        Ok(match op {
            "ci_claim" if first.swap(false, Ordering::SeqCst) => {
                json!({ "won": false, "deferred": "another computer first" })
            }
            "ci_claim" => json!({ "won": true, "ci": crate::agent_ci::timing::CiTiming::test_json() }),
            "ci_merge_requests" => json!({ "requests": [], "ci": crate::agent_ci::timing::CiTiming::test_json() }),
            _ => json!({ "ok": true }),
        })
    });
    store.ask_reruns(ask(10));
    r.ci = std::mem::replace(&mut r.ci, CiLoop::new(Programs::default())).with_store(store.clone());
    r.store = store;
    poll_and_start(&mut r).await;
    assert_eq!(docker_runs(&r), 0, "the store left it to another computer");
    poll_and_start(&mut r).await;
    assert_eq!(docker_runs(&r), 1, "the next poll takes it: {}", calls(&r));
}

#[tokio::test]
async fn a_push_to_main_is_never_asked_a_rerun_of_and_an_unreadable_store_asks_none() {
    let mut r = pr_rig("exit 0");
    finished(&mut r);
    r.store.ask_reruns(ask(10));
    r.ci.poll(&r.cfg).await;
    let merge = r.ci.merge.clone().unwrap();
    assert_eq!(merge.rerun("o/p", 7, "h7"), Some(10));
    assert_eq!(merge.rerun("o/p", 8, "h7"), None);
    assert_eq!(merge.rerun("o/other", 7, "h7"), None);
}
