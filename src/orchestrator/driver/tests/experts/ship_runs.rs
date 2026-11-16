//! Shipping an expert session in `runs` mode (`driver/experts/ship_runs.rs`): its change is handed to the store as
//! the item's piece (`expert_ship`), the session and its lane are let go, and the item loop never takes it. What the
//! store is owed survives a lost reply and a restart.
use super::*;
use crate::backlog::expert::ShipOwed;
use crate::orchestrator::driver::experts::Experts;

const TARGET: &str = "retaskable/expert-fix-it-fix-it-s0";

/// A session in `runs` mode with its ship asked for, as command `e1`.
async fn asked() -> Ex {
    let mut x = ex().await;
    started(&mut x).await;
    x.rig.go_runs();
    x.end("e1", ID, "ship");
    x
}

/// Runs passes until `ok`, or says what never came.
async fn until_runs(x: &mut Ex, what: &str, ok: impl Fn(&mut Ex) -> bool) {
    for _ in 0..300 {
        x.rig.runs_pass().await;
        x.drain();
        if ok(x) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

async fn answered(x: &mut Ex, id: &str) -> Value {
    until_runs(x, "the command is answered", |x| x.heard.iter().any(|a| a["id"] == id)).await;
    x.heard.iter().find(|a| a["id"] == id).cloned().unwrap()
}

fn ship_args(x: &Ex) -> Vec<Value> {
    x.st.args("expert_ship")
}

#[tokio::test]
async fn in_runs_mode_the_change_goes_to_the_store_as_a_piece_and_the_session_and_its_lane_are_let_go() {
    let mut x = asked().await;
    let a = answered(&mut x, "e1").await;
    assert_eq!(a["type"], "ack", "{a}");
    let job = x.host.shipped.lock().unwrap().remove(0);
    assert_eq!((job.target.as_str(), job.prepare), (TARGET, false));
    // The call carries the session's lease, the branch the runs will check out, and the change.
    assert_eq!(
        ship_args(&x),
        [json!({
            "id": ID, "token": format!("lease-{ID}"), "branch": TARGET,
            "prUrl": "https://github.com/o/r/pull/9", "prNumber": 9, "head": "a".repeat(40), "title": "Fix it",
        })]
    );
    // No claim-mode marker, and no note of ours (the store writes "Shipped by ..." itself, and the lease is gone).
    assert!(x.st.args("report").iter().all(|r| r.get("detail").is_none()));
    assert_eq!(x.st.count("expert_queue_clear"), 1);
    assert_eq!(x.st.count("release"), 0);
    // Let go: no session, no lane, and nothing the item loop took over or listed as owed.
    assert!(x.expert(ID).is_none());
    assert_eq!(x.lane_of(ID), None);
    let lp = x.rig.lp(0);
    assert!(!lp.runs.contains_key(ID) && !lp.items.contains_key(ID) && !lp.leases.contains_key(ID));
    assert!(lp.experts.owed.is_empty() && lp.store.ship_owed().is_empty());
}

#[tokio::test]
async fn a_parked_session_takes_a_lane_to_ship_and_frees_it() {
    let mut x = ex().await;
    started(&mut x).await;
    x.end("e0", ID, "park");
    assert_eq!(x.answer("e0").await["type"], "ack");
    x.rig.go_runs();
    x.end("e1", ID, "ship");
    assert_eq!(answered(&mut x, "e1").await["type"], "ack");
    assert!(x.host.shipped.lock().unwrap()[0].prepare);
    assert_eq!(x.lane_of(ID), None);
    assert_eq!(ship_args(&x).len(), 1);
}

#[tokio::test]
async fn a_refusal_is_said_in_words_and_the_session_is_as_it_was_with_its_change_open() {
    for code in ["forbidden", "conflict", "stale_lease", "not_found", "bad_request"] {
        let mut x = asked().await;
        *x.st.ship_op_refused.lock().unwrap() = Some(code);
        let a = answered(&mut x, "e1").await;
        assert_eq!(a["type"], "nack", "{code}: {a}");
        let why = a["reason"].as_str().unwrap();
        assert!(
            why.contains("https://github.com/o/r/pull/9")
                && why.contains("wouldn't take it as the item's piece")
                && why.contains(code)
                && why.contains("as it was"),
            "{code}: {why}"
        );
        assert_eq!(x.detail(ID).as_deref(), Some("expert_idle"));
        assert!(x.lane_of(ID).is_some());
        assert_eq!(x.st.count("expert_queue_clear"), 0);
        // A refusal can't change by being sent again, so nothing stays owed.
        assert!(x.rig.lp(0).experts.owed.is_empty() && x.rig.lp(0).store.ship_owed().is_empty());
        x.quiet().await;
        assert_eq!(ship_args(&x).len(), 1, "{code}: sent once");
    }
}

#[tokio::test]
async fn a_lost_reply_is_sent_again_with_the_same_call() {
    let mut x = asked().await;
    x.st.ship_reply_lost.store(true, Ordering::SeqCst);
    // The store took it and the reply was lost: the person is told it went, the session lets go and the call stays owed.
    assert_eq!(answered(&mut x, "e1").await["type"], "ack");
    until_runs(&mut x, "the call is sent again and taken", |x| {
        ship_args(x).len() == 2 && x.rig.lp(0).experts.owed.is_empty()
    })
    .await;
    let sent = ship_args(&x);
    assert_eq!(sent[0], sent[1]);
    assert_eq!(x.st.count("expert_queue_clear"), 1);
    assert!(x.rig.lp(0).store.ship_owed().is_empty());
}

#[tokio::test]
async fn a_restart_between_the_change_and_the_op_sends_it_from_the_state_file() {
    let mut x = asked().await;
    *x.st.ship_op_refused.lock().unwrap() = Some("down");
    assert_eq!(answered(&mut x, "e1").await["type"], "ack");
    let owed: ShipOwed = x.rig.lp(0).store.ship_owed()[ID].clone();
    assert_eq!(
        (owed.lease.as_str(), owed.branch.as_str(), owed.pr_number),
        (format!("lease-{ID}").as_str(), TARGET, 9)
    );
    // A restart: the session was in memory only, so what is left is what the state file saved.
    let lp = &mut x.rig.machine.projects[0];
    lp.experts = Experts::with_owed(lp.store.ship_owed());
    *x.st.ship_op_refused.lock().unwrap() = None;
    until_runs(&mut x, "the call goes after the restart", |x| {
        x.rig.lp(0).experts.owed.is_empty()
    })
    .await;
    let sent = ship_args(&x);
    assert!(sent.len() >= 2);
    assert_eq!(sent.last().unwrap()["prUrl"], "https://github.com/o/r/pull/9");
    assert_eq!(sent.last().unwrap()["token"], format!("lease-{ID}"));
    assert!(x.rig.lp(0).store.ship_owed().is_empty());
    assert_eq!(x.st.count("expert_queue_clear"), 1);
}
