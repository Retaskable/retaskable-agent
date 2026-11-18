//! The loop's half of an item's merge: the store's turn before the step, `merge_result` after it, and the completion, through the
//! outbox, which waits out an outage and a restart.
use super::super::flow::checking;
use super::super::restart::restarted;
use super::super::*;
use crate::lane_pool::Holder;
use crate::orchestrator::steps::MergeEnd;
use crate::store::FlowRecord;
use std::sync::atomic::{AtomicBool, Ordering};

const PR: &str = "https://github.com/o/p/pull/7";

/// A store that answers the merge queue's ops (the turn is item "a"'s, its beat holds) and `complete`, recording every op, and
/// refuses those with a 500 while `down` is set.
pub(super) async fn merge_store(listed: Value, down: Arc<AtomicBool>) -> (String, Asked) {
    merge_store_with(listed, down, Arc::default()).await
}

/// `merge_store` that also refuses `tryout_add` with a 500 while `tryout_down` is set.
pub(super) async fn merge_store_with(
    listed: Value,
    down: Arc<AtomicBool>,
    tryout_down: Arc<AtomicBool>,
) -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, listed, down, tryout_down) = (seen.clone(), listed.clone(), down.clone(), tryout_down.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let quiet = ["settings_get", "pause_get", "facts_get", "details_get", "details_report"];
                if quiet.contains(&op.as_str()) {
                    return (StatusCode::OK, Json(json!({ "value": if op == "pause_get" { json!({ "paused": null, "log": [] }) } else { Value::Null } })));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "complete" => json!({ "built": false, "status": "todo" }),
                    "merge_enqueue" | "merge_withdraw" | "park" | "release" | "report" => json!({}),
                    "merge_next" => json!({ "turn": { "token": "turn-1", "entry": { "item": "a" } } }),
                    "merge_beat" | "merge_result" => json!({ "held": true }),
                    "merge_open" => json!({}),
                    "health" => json!({ "tasks": [], "stop": [], "free": 1, "allowed": 2, "tuning": crate::store_tuning::StoreTuning::test_json() }),
                    _ => listed,
                };
                if ((op.starts_with("merge_") || op == "complete") && down.load(Ordering::SeqCst))
                    || (op == "tryout_add" && tryout_down.load(Ordering::SeqCst))
                {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })));
                }
                (StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

pub(super) fn ops(asked: &Asked, name: &str) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == name)
        .map(|(_, a)| a.clone())
        .collect()
}

pub(super) fn merged(commit: &str) -> StepDone {
    let mut run = ItemRun::new();
    run.state = ItemState::Merged;
    StepDone {
        run,
        merge: Some(MergeEnd::Merged {
            commit: Some(commit.into()),
        }),
        notes: vec!["Merged pull request #7".into()],
        ..Default::default()
    }
}

/// A live rig whose item "a" is `merging` with no lane, approved and with its change known.
pub(super) async fn merging(down: Arc<AtomicBool>) -> (Rig, Arc<Fake>, Asked) {
    merging_with(down, Arc::default()).await
}

pub(super) async fn merging_with(down: Arc<AtomicBool>, tryout_down: Arc<AtomicBool>) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = merge_store_with(json!([item("a", "merging")]), down, tryout_down).await;
    let (mut rig, fake) = checking(&base).await;
    at(&mut rig, 0, "a", ItemState::Merging, None);
    rig.machine.pool.release(
        1,
        &Holder::Item {
            project: "p0".into(),
            item: "a".into(),
        },
    );
    let record = rig.lp(0).records.get_mut("a").unwrap();
    record.lane = None;
    record.state = "merging".into();
    record.flow = FlowRecord {
        pr_url: Some(PR.into()),
        pr_number: Some(7),
        approved_sha: Some("abc".into()),
        ..Default::default()
    };
    (rig, fake, asked)
}

#[tokio::test]
async fn the_step_runs_only_with_the_stores_turn_and_a_landed_change_is_reported_and_completed() {
    let (mut rig, fake, asked) = merging(Arc::default()).await;
    *fake.done.lock().unwrap() = Some(merged("deadbeef"));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    let calls = super::super::flow::calls(&asked);
    let at = |op: &str| {
        calls
            .iter()
            .position(|c| c == op)
            .unwrap_or_else(|| panic!("no {op} in {calls:?}"))
    };
    assert!(
        at("merge_enqueue") < at("merge_next")
            && at("merge_next") < at("merge_beat")
            && at("merge_beat") < at("merge_result")
    );
    let enqueue = &ops(&asked, "merge_enqueue")[0];
    assert_eq!(
        (
            enqueue["item"].as_str(),
            enqueue["pr"]["number"].as_u64(),
            enqueue["approvedSha"].as_str()
        ),
        (Some("a"), Some(7), Some("abc"))
    );
    assert_eq!(enqueue.get("chore"), None);
    let result = &ops(&asked, "merge_result")[0];
    assert_eq!(
        (
            result["token"].as_str(),
            result["outcome"].as_str(),
            result["mergeCommit"].as_str()
        ),
        (Some("turn-1"), Some("merged"), Some("deadbeef"))
    );
    // The completion: the implementer said nothing of the whole item here, so it is not built and goes back to the queue.
    let done = &ops(&asked, "complete");
    assert_eq!(done.len(), 1, "{calls:?}");
    assert_eq!(
        (
            done[0]["id"].as_str(),
            done[0]["token"].as_str(),
            done[0]["built"].as_bool()
        ),
        (Some("a"), Some("lease-1"), Some(false))
    );
    let lp = rig.lp(0);
    assert!(lp.runs.is_empty() && lp.leases.is_empty() && lp.outbox.is_empty());
    assert!(!lp.turns.may_merge("a"));
    // The slice's `merged` event: the pull request, the merge commit and how it landed.
    let events: Vec<Value> = lp.events().recent().iter().map(Entry::to_value).collect();
    let merged: Vec<&Value> = events.iter().filter(|e| e["kind"] == "merged").collect();
    assert_eq!(merged.len(), 1, "{events:?}");
    assert_eq!(
        (
            merged[0]["item"].as_str(),
            merged[0]["pr"].as_u64(),
            merged[0]["commit"].as_str()
        ),
        (Some("a"), Some(7), Some("deadbeef"))
    );
    assert_eq!(
        (merged[0]["how"].as_str(), merged[0]["chore"].as_bool()),
        (Some("merge"), Some(false))
    );
}

#[tokio::test]
async fn without_the_turn_nothing_is_stepped_and_a_store_that_cannot_be_asked_is_a_wait() {
    let down = Arc::new(AtomicBool::new(true));
    let (mut rig, fake, _asked) = merging(down.clone()).await;
    for _ in 0..3 {
        rig.machine.pass().await;
        settle().await;
    }
    assert!(
        fake.stepped.lock().unwrap().is_empty(),
        "stepped without the store's turn"
    );
    down.store(false, Ordering::SeqCst);
    *fake.done.lock().unwrap() = Some(merged("c1"));
    rig.machine.pass().await;
    settle().await;
    assert_eq!(fake.stepped.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_completion_the_store_cannot_take_is_kept_across_the_outage_and_a_restart_until_it_answers() {
    let down = Arc::new(AtomicBool::new(false));
    let (mut rig, fake, asked) = merging(down.clone()).await;
    *fake.done.lock().unwrap() = Some(merged("deadbeef"));
    rig.machine.pass().await;
    settle().await;
    down.store(true, Ordering::SeqCst);
    rig.machine.pass().await;
    rig.machine.pass().await;
    // Merged on the host, completion owed: listed and saved with its lease, and the lease kept.
    let saved = Store::load(&rig._dir.path().join("0")).unwrap();
    assert_eq!(saved.complete_outbox()["a"].lease, "lease-1");
    assert_eq!(saved.complete_outbox()["a"].completion.merge_commit, None);
    down.store(false, Ordering::SeqCst);
    let (base, _) = merge_store(json!([item("a", "merging")]), Arc::default()).await;
    let mut rig = restarted(rig, &base);
    rig.go_live();
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(
        rig.lp(0).outbox.is_empty(),
        "the completion was not sent after the restart"
    );
    let _ = asked;
}
