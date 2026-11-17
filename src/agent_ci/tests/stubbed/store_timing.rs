//! docs/specs/agent-ci.md section 3.5: the timing is the store's (`ci` in `ci_merge_requests`' and `ci_claim`'s answers). A repo whose
//! timing can't be read starts no check; the deal, the hold and the poll follow what the store says.
use super::*;
use serde_json::Value;

/// `r` with a store that answers `ci_merge_requests` with `ci` (`None`: an older store, no `ci`) and wins every claim.
fn store_saying(r: &mut Rig, ci: Option<Value>) -> Arc<FakeStore> {
    let store = FakeStore::answering(move |op, _| {
        Ok(match op {
            "ci_claim" => json!({ "won": true }),
            "ci_merge_requests" => match &ci {
                Some(ci) => json!({ "requests": [], "ci": ci }),
                None => json!({ "requests": [] }),
            },
            _ => json!({ "ok": true }),
        })
    });
    r.ci = std::mem::replace(&mut r.ci, CiLoop::new(Programs::default())).with_store(store.clone());
    store
}

fn ci_with(change: impl FnOnce(&mut Value)) -> Option<Value> {
    let mut ci = timing::CiTiming::test_json();
    change(&mut ci);
    Some(ci)
}

#[tokio::test]
async fn no_check_starts_while_the_stores_timing_cannot_be_read_and_one_starts_once_it_can() {
    let mut r = pr_rig("exit 0");
    claim_gh(&mut r, &[]);
    let store = store_saying(&mut r, None);
    r.ci.poll(&r.cfg).await;
    let keys = || [("o-p".to_string(), vec![key(7, "h7")])].into();
    assert!(
        r.ci.start(&r.cfg, keys()).await.is_empty(),
        "an older store sends no `ci`"
    );
    assert_eq!(docker_runs(&r), 0);
    assert!(store.args("ci_claim").is_empty(), "not even a claim");
    assert_eq!(
        r.ci.poll_pause(&r.cfg),
        UNKNOWN_PAUSE,
        "nothing is known, so only the pause between tries at reading it"
    );
    // The store sends its timing: the next poll reads it and the check runs.
    store_saying(&mut r, ci_with(|_| {}));
    r.ci.poll(&r.cfg).await;
    for t in r.ci.start(&r.cfg, keys()).await {
        t.await.unwrap();
    }
    assert_eq!(docker_runs(&r), 1);
}

#[tokio::test]
async fn a_store_that_cannot_be_read_leaves_the_timing_unknown_and_starts_nothing() {
    let mut r = pr_rig("exit 0");
    claim_gh(&mut r, &[]);
    let down = FakeStore::answering(|_, _| Err("down".into()));
    r.ci = std::mem::replace(&mut r.ci, CiLoop::new(Programs::default())).with_store(down);
    r.ci.poll(&r.cfg).await;
    assert!(
        r.ci.start(&r.cfg, [("o-p".to_string(), vec![key(7, "h7")])].into())
            .await
            .is_empty()
    );
    assert_eq!(docker_runs(&r), 0);
}

#[tokio::test]
async fn the_poll_pause_is_the_stores_poll_seconds_and_a_change_applies_at_the_next_poll() {
    let mut r = pr_rig("exit 0");
    claim_gh(&mut r, &[]);
    store_saying(&mut r, ci_with(|ci| ci["ciPollSeconds"] = json!(45)));
    r.ci.poll(&r.cfg).await;
    assert_eq!(r.ci.poll_pause(&r.cfg), Duration::from_secs(45));
    store_saying(&mut r, ci_with(|ci| ci["ciPollSeconds"] = json!(7)));
    r.ci.poll(&r.cfg).await;
    assert_eq!(r.ci.poll_pause(&r.cfg), Duration::from_secs(7));
}

#[tokio::test]
async fn the_deal_gives_each_repo_the_stores_number_of_checks_per_turn() {
    // Two permits and two repos with plenty waiting: one each when the store says 1, both to the first repo when it says 2.
    for (per_turn, want) in [(1, (1, 1)), (2, (2, 0))] {
        let mut r = pr_rig("wait_gate");
        r.cfg.repos.push(Repo::parse("o/q").unwrap());
        r.cfg.slots = 2;
        store_saying(&mut r, ci_with(|ci| ci["ciDealPerTurn"] = json!(per_turn)));
        r.ci.poll(&r.cfg).await;
        let keys = |from: u64| (from..from + 4).map(|i| key(i, &format!("h{i}"))).collect::<Vec<_>>();
        let tasks =
            r.ci.start(&r.cfg, [("o-p".into(), keys(100)), ("o-q".into(), keys(200))].into())
                .await;
        let running = |id: &str| r.ci.running.lock().unwrap().iter().filter(|(i, _)| i == id).count();
        assert_eq!((running("o-p"), running("o-q")), want, "{per_turn} per turn");
        open_gate(&r);
        for t in tasks {
            t.await.unwrap();
        }
    }
}
