//! Picking up a rotation chore when a project is otherwise idle, and dropping one instead of letting
//! it wait on a person (`driver/chores.rs`, `driver/settle.rs`'s `drop_chore`).
use super::*;
use crate::config::OnOff;
use crate::orchestrator::driver::chores::ChoreInFlight;

fn chore_answer(kind: &str, lock: &str) -> Value {
    json!({ "kind": kind, "lock": lock, "started": 1, "perDay": 3, "resetsAt": "2026-10-30T00:00:00Z" })
}

/// `claim_stub`, but `chore_next` answers `chore` and `chore_lock` always succeeds; `claim_next` always
/// answers nothing, since these tests give a project nothing else to claim from the roadmap.
async fn chore_stub(listed: Value, chore: Option<Value>) -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, listed, chore) = (seen.clone(), listed.clone(), chore.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                match op.as_str() {
                    "claim_next" => (StatusCode::OK, Json(json!({ "value": null }))),
                    "chore_next" => (StatusCode::OK, Json(json!({ "value": chore }))),
                    "release" | "park" => (StatusCode::OK, Json(json!({}))),
                    "chore_lock" => (StatusCode::OK, Json(json!({ "value": { "held": false } }))),
                    _ => (StatusCode::OK, Json(json!({ "value": listed }))),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

/// `chore_stub` whose `chore_next` always fails.
async fn chore_stub_erroring() -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                match op.as_str() {
                    "claim_next" => (StatusCode::OK, Json(json!({ "value": null }))),
                    "chore_next" => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                    _ => (StatusCode::OK, Json(json!({ "value": [] }))),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

/// Calls `claim_chore` directly, bypassing the rest of a pass, for the guard tests that don't need one.
async fn claim_chore_now(rig: &mut Rig, slots: &mut u32, starter: &dyn SessionStarter) {
    let lp = &mut rig.machine.projects[0];
    let pool = &mut rig.machine.pool;
    let links = &rig.machine.links;
    lp.claim_chore(pool, slots, links, starter).await;
}

#[tokio::test]
async fn claim_chore_starts_the_rotations_next_kind_when_the_project_is_otherwise_idle() {
    let (base, asked) = chore_stub(json!([]), Some(chore_answer("tidy", "chore-tidy"))).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    let fake = rig.go_live();
    rig.machine.pass().await;

    assert_eq!(ops(&asked), ["list", "claim_next", "chore_next"]);
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(
        (started.len(), started[0].item_id.as_str(), started[0].lane),
        (1, "chore-1", 1)
    );
    assert_eq!(rig.lp(0).runs["chore-1"].state, ItemState::Implementing);
    assert!(rig.lp(0).runs["chore-1"].chore);
    assert_eq!(rig.machine.pool.lane_of("p0", "chore-1"), Some(1));
    assert_eq!(rig.lp(0).items["chore-1"].kind.as_deref(), Some("chore:tidy"));
    assert_eq!(rig.lp(0).items["chore-1"].title, "chore-1: Tidy up");
    assert_eq!(
        rig.lp(0).chore_inflight,
        Some(ChoreInFlight {
            item_id: "chore-1".into(),
            kind: "tidy".into(),
            lock: "chore-tidy".into(),
        })
    );
    assert_eq!(rig.lp(0).store.chores().next_id, 1);
    assert!(
        !rig.lp(0).leases.contains_key("chore-1"),
        "a chore is local only to the agent, never leased from the backlog"
    );
}

#[tokio::test]
async fn claim_chore_does_nothing_when_chores_are_off() {
    let (base, asked) = chore_stub(json!([]), Some(chore_answer("tidy", "chore-tidy"))).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.pass().await;
    assert!(!ops(&asked).contains(&"chore_next".to_string()));
    assert!(rig.lp(0).chore_inflight.is_none());
}

#[tokio::test]
async fn claim_chore_does_nothing_while_a_chore_is_already_in_flight() {
    let (base, asked) = chore_stub(json!([]), Some(chore_answer("tidy", "chore-tidy"))).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    rig.lp(0).chore_inflight = Some(ChoreInFlight {
        item_id: "chore-1".into(),
        kind: "tidy".into(),
        lock: "chore-tidy".into(),
    });
    let fake = rig.go_live();
    let mut slots = 8;
    claim_chore_now(&mut rig, &mut slots, fake.as_ref()).await;
    assert!(!ops(&asked).contains(&"chore_next".to_string()));
}

#[tokio::test]
async fn claim_chore_does_nothing_while_something_is_still_queued() {
    let (base, asked) = chore_stub(json!([]), Some(chore_answer("tidy", "chore-tidy"))).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Queued, None);
    let mut slots = 8;
    claim_chore_now(&mut rig, &mut slots, fake.as_ref()).await;
    assert!(
        !ops(&asked).contains(&"chore_next".to_string()),
        "a chore never takes a turn a queued roadmap item still waits for"
    );
    assert!(rig.lp(0).chore_inflight.is_none());
}

#[tokio::test]
async fn claim_chore_does_nothing_when_no_lane_is_free() {
    let (base, asked) = chore_stub(json!([]), Some(chore_answer("tidy", "chore-tidy"))).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    let fake = rig.go_live();
    for lane in 1..=2 {
        assert!(rig.machine.pool.take(
            lane,
            Holder::Item {
                project: "other".into(),
                item: "x".into(),
            }
        ));
    }
    let mut slots = 8;
    claim_chore_now(&mut rig, &mut slots, fake.as_ref()).await;
    assert!(!ops(&asked).contains(&"chore_next".to_string()));
}

#[tokio::test]
async fn claim_chore_skips_an_unknown_kind() {
    let (base, _asked) = chore_stub(json!([]), Some(chore_answer("mystery", "chore-mystery"))).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    let fake = rig.go_live();
    rig.machine.pass().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert!(rig.lp(0).chore_inflight.is_none());
    assert!(rig.lp(0).runs.is_empty());
    assert_eq!(
        rig.lp(0).store.chores().next_id,
        0,
        "no id was taken for a kind this agent doesn't know"
    );
}

#[tokio::test]
async fn a_store_error_on_chore_next_starts_nothing_and_is_an_error_event() {
    let (base, asked) = chore_stub_erroring().await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    let fake = rig.go_live();
    rig.machine.pass().await;
    assert_eq!(ops(&asked), ["list", "claim_next", "chore_next"]);
    assert!(fake.started.lock().unwrap().is_empty());
    assert!(rig.lp(0).chore_inflight.is_none());
    assert!(rig.lp(0).events().recent().iter().any(|e| e.kind() == "store_error"));
}

/// A rig that's claimed the rotation's next chore ("chore-1" on a free lane), with its session
/// scripted to end at once with `result`.
async fn chore_ended(result: SessionResult) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = chore_stub(json!([]), Some(chore_answer("tidy", "chore-tidy"))).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.chores = OnOff::On;
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(result);
    rig.machine.pass().await;
    (rig, fake, asked)
}

#[tokio::test]
async fn a_failed_chore_session_is_dropped_not_handed_back_and_its_lock_released() {
    let (mut rig, _fake, asked) = chore_ended(SessionResult::Failed("boom".into())).await;
    rig.machine.pass().await;

    assert!(!rig.lp(0).runs.contains_key("chore-1"));
    assert!(!rig.lp(0).items.contains_key("chore-1"));
    assert_eq!(rig.machine.pool.lane_of("p0", "chore-1"), None);
    assert_ne!(
        rig.lp(0).chore_inflight.as_ref().map(|c| c.item_id.as_str()),
        Some("chore-1"),
        "chore-1 itself is no longer in flight (the rotation may already be onto its next chore)"
    );
    assert!(
        !ops(&asked).contains(&"release".to_string()),
        "a chore was never leased, so there's nothing to release"
    );

    let lock_call = asked.lock().unwrap().iter().find(|(op, _)| op == "chore_lock").cloned();
    assert_eq!(
        lock_call.map(|(_, args)| args),
        Some(json!({ "name": "chore-tidy", "take": false }))
    );

    let history = rig.lp(0).store.chores().history;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, "chore-1");
    assert_eq!(history[0].kind, "tidy");
    assert_eq!(history[0].outcome, "dropped");
    assert_eq!(history[0].title, "chore-1: Tidy up");
}

#[tokio::test]
async fn a_chore_session_that_needs_a_human_is_dropped_too() {
    let (mut rig, _fake, asked) = chore_ended(SessionResult::NeedsHuman("gave up".into())).await;
    rig.machine.pass().await;
    assert!(!rig.lp(0).runs.contains_key("chore-1"));
    assert_eq!(rig.lp(0).store.chores().history.last().unwrap().outcome, "dropped");
    assert!(ops(&asked).contains(&"chore_lock".to_string()));
}

#[tokio::test]
async fn a_chore_left_needing_a_person_is_dropped_not_parked() {
    let asking = ImplementerOutcome::Questions {
        all_already_answered: false,
    };
    let (mut rig, _fake, asked) = chore_ended(SessionResult::Implementer(asking)).await;
    rig.machine.pass().await;
    assert!(!rig.lp(0).runs.contains_key("chore-1"));
    assert!(
        !ops(&asked).contains(&"park".to_string()),
        "a chore never waits on a person: it's dropped instead"
    );
    assert_eq!(rig.lp(0).store.chores().history.last().unwrap().outcome, "dropped");
}

#[tokio::test]
async fn a_finished_chore_slice_still_in_progress_is_stepped_not_dropped() {
    let (mut rig, _fake, _asked) = chore_ended(SessionResult::Implementer(ImplementerOutcome::Finished)).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["chore-1"].state, ItemState::Checking);
    assert!(
        rig.lp(0).chore_inflight.is_some(),
        "still in flight: the chore isn't done yet"
    );
}

#[tokio::test]
async fn finish_chore_does_nothing_for_an_item_that_isnt_the_one_in_flight() {
    let (base, _asked) = chore_stub(json!([]), None).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).chore_inflight = Some(ChoreInFlight {
        item_id: "chore-1".into(),
        kind: "tidy".into(),
        lock: "chore-tidy".into(),
    });
    let lp = &mut rig.machine.projects[0];
    let links = &rig.machine.links;
    lp.finish_chore("chore-2", "Some other chore", "dropped", links).await;
    assert!(
        rig.lp(0).chore_inflight.is_some(),
        "only the in-flight chore's own id ends it"
    );
    assert!(rig.lp(0).store.chores().history.is_empty());
}
