//! Shipping an expert session (`driver/experts/ship.rs`): its change opened, and the item handed to the project's item
//! loop in `checking`, with the lease and the lane the session held.
use super::*;
use crate::item_lifecycle::ItemState;

const TARGET: &str = "retaskable/expert-fix-it-fix-it-s0";

async fn shipped(x: &mut Ex) {
    started(x).await;
    x.end("e1", ID, "ship");
    let a = x.answer("e1").await;
    assert_eq!(a["type"], "ack", "{a}");
}

fn reports(x: &Ex) -> Vec<Value> {
    x.st.args("report")
        .into_iter()
        .filter(|a| a["detail"].is_string())
        .collect()
}

#[tokio::test]
async fn shipping_opens_the_change_and_the_item_loop_takes_the_item_over_in_checking() {
    let mut x = ex().await;
    shipped(&mut x).await;
    let job = x.host.shipped.lock().unwrap().remove(0);
    assert_eq!(
        (
            job.title.as_str(),
            job.branch.as_str(),
            job.target.as_str(),
            job.prepare
        ),
        (
            "expert-fix-it: Fix it",
            "retaskable/expert-fix-it-fix-it",
            TARGET,
            false
        )
    );
    assert_eq!(job.body, "Fix it\n\nDriven by ada in a Retaskable expert session.");
    // The store hears the marker before anything is pushed, and the change's address once it is open.
    let told = reports(&x);
    assert_eq!(told.len(), 2);
    assert_eq!(
        (
            told[0]["status"].as_str(),
            told[0]["detail"].as_str(),
            told[0]["branch"].as_str(),
            told[0].get("prUrl")
        ),
        (Some("in_review"), Some("expert_shipped"), Some(TARGET), None)
    );
    assert_eq!(told[1]["prUrl"], "https://github.com/o/r/pull/9");
    assert_eq!(told[1]["token"], format!("lease-{ID}"));
    assert!(x.st.args("report").iter().any(|a| a["note"] == "Shipped by the person"));
    assert_eq!(x.st.count("expert_queue_clear"), 1);
    assert_eq!(
        x.st.count("release"),
        0,
        "the lease stays: this computer carries the change on"
    );
    // The session is gone and the item is the item loop's: checking, on the same lane, under the same lease.
    assert!(x.expert(ID).is_none());
    let lane = x.lane_of(ID).expect("it keeps its lane for the checks");
    let lp = x.rig.lp(0);
    assert_eq!(lp.runs[ID].state, ItemState::Checking);
    assert_eq!(lp.leases[ID], format!("lease-{ID}"));
    let record = &lp.records[ID];
    assert_eq!(
        (record.state.as_str(), record.lane, record.branch.as_deref()),
        ("checking", Some(lane), Some(TARGET))
    );
    // Saved, so a restart takes it back as any item.
    assert_eq!(lp.store.leases()[ID], format!("lease-{ID}"));
}

#[tokio::test]
async fn a_parked_session_takes_a_lane_again_and_ships_from_its_pushed_branch() {
    let mut x = ex().await;
    started(&mut x).await;
    x.end("e0", ID, "park");
    assert_eq!(x.answer("e0").await["type"], "ack");
    assert_eq!(x.lane_of(ID), None);
    x.end("e1", ID, "ship");
    assert_eq!(x.answer("e1").await["type"], "ack");
    assert!(
        x.host.shipped.lock().unwrap()[0].prepare,
        "its lane is set up from the pushed branch first"
    );
    assert!(x.lane_of(ID).is_some());
}

#[tokio::test]
async fn a_change_that_cant_be_opened_leaves_the_session_as_it_was_and_the_marker_rolled_back() {
    let mut x = ex().await;
    started(&mut x).await;
    *x.host.ship_refused.lock().unwrap() = Some("gh pr create failed: no".into());
    x.end("e1", ID, "ship");
    let a = x.answer("e1").await;
    assert_eq!(a["type"], "nack");
    assert_eq!(
        a["reason"],
        "Couldn't push retaskable/expert-fix-it-fix-it or open its pull request: gh pr create failed: no"
    );
    let told = reports(&x);
    assert_eq!(told.last().unwrap()["status"], "claimed");
    assert_eq!(told.last().unwrap()["detail"], "");
    assert_eq!(x.detail(ID).as_deref(), Some("expert_idle"));
    assert!(x.lane_of(ID).is_some());
    assert!(!x.rig.lp(0).runs.contains_key(ID), "the item loop didn't take it");
}

#[tokio::test]
async fn a_parked_ship_that_fails_gives_its_lane_back() {
    let mut x = ex().await;
    started(&mut x).await;
    x.end("e0", ID, "park");
    x.answer("e0").await;
    *x.host.ship_refused.lock().unwrap() = Some("push refused".into());
    x.end("e1", ID, "ship");
    assert_eq!(x.answer("e1").await["type"], "nack");
    assert_eq!(x.lane_of(ID), None);
    assert_eq!(x.detail(ID).as_deref(), Some("expert_parked"));
}

#[tokio::test]
async fn after_a_restart_the_item_loop_takes_the_shipped_item_back_in_checking() {
    let mut x = ex().await;
    shipped(&mut x).await;
    let lane = x.lane_of(ID).unwrap();
    // A restart: the pool is new and the loop has only what its state file saved and what the store says.
    let holder = x.rig.machine.pool.holder(lane).cloned().unwrap();
    x.rig.machine.pool.release(lane, &holder);
    let lp = &mut x.rig.machine.projects[0];
    (lp.adopted, lp.runs, lp.items, lp.leases) = (false, Default::default(), Default::default(), lp.store.leases());
    // The store lists it as an expert item with the marker, which the loop's read lets through.
    let listed: Vec<crate::backlog::Item> = serde_json::from_value(json!([{
        "id": ID, "title": "Fix it", "status": "in_review", "rank": 1, "detail": "expert_shipped", "kind": "expert"
    }]))
    .unwrap();
    let kept: Vec<_> = listed
        .into_iter()
        .filter(|i| !crate::expert::held_by_experts(&i.id, i.kind.as_deref(), i.detail.as_deref()))
        .collect();
    assert_eq!(kept.len(), 1);
    x.rig.machine.projects[0].readopt(&kept, &mut x.rig.machine.pool, "my-pc");
    assert_eq!(x.rig.machine.projects[0].runs[ID].state, ItemState::Checking);
    assert_eq!(x.lane_of(ID), Some(lane));
}
