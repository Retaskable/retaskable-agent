//! A person's playtest answer on an `awaiting_playtest` item (`driver/item_commands.rs`).
use super::*;
use crate::view::ItemCommand;

/// A live rig whose item "a" waits for a playtest on branch "a", with `lanes` lanes and one review round used.
fn awaiting(lanes: u32) -> (Rig, Arc<Fake>) {
    let mut rig = rig(lanes, &["http://127.0.0.1:1"]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    let lp = rig.lp(0);
    let run = lp.runs.get_mut("a").unwrap();
    run.state = ItemState::AwaitingPlaytest;
    run.review_rounds = 1;
    lp.leases.insert("a".into(), "lease-1".into());
    lp.records.insert(
        "a".into(),
        RunRecord {
            state: "awaiting_playtest".into(),
            impl_session_id: Some("s1".into()),
            branch: Some("a".into()),
            ..Default::default()
        },
    );
    (rig, fake)
}

fn answer(rig: &Rig, item: &str, passed: bool, notes: Option<&str>) {
    rig.machine.links.item_commands.push(ItemCommand::Playtest {
        project: "p0".into(),
        id: item.into(),
        passed,
        notes: notes.map(Into::into),
        merged_at: None,
    });
}

#[tokio::test]
async fn a_pass_moves_the_item_to_merging_and_starts_no_session() {
    let (mut rig, fake) = awaiting(2);
    answer(&rig, "a", true, None);
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Merging);
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_fail_resumes_the_implementer_on_its_branch_with_the_notes() {
    let (mut rig, fake) = awaiting(2);
    answer(&rig, "a", false, Some("The button does nothing."));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert!(
        started[0]
            .follow_up
            .as_deref()
            .unwrap()
            .contains("The button does nothing.")
    );
    drop(started);
    let run = &rig.lp(0).runs["a"];
    assert_eq!((run.state, run.review_rounds), (ItemState::Implementing, 2));
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
}

#[tokio::test]
async fn a_fail_with_blank_notes_still_resumes() {
    let (mut rig, fake) = awaiting(2);
    answer(&rig, "a", false, Some("  "));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert!(started[0].follow_up.as_deref().unwrap().contains("found a problem"));
}

#[tokio::test]
async fn a_fail_past_the_review_rounds_stops_the_item_and_starts_nothing() {
    let (mut rig, fake) = awaiting(2);
    let max = rig.lp(0).project.max_review_rounds;
    rig.lp(0).runs.get_mut("a").unwrap().review_rounds = max;
    answer(&rig, "a", false, Some("Broken."));
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_fail_with_no_free_lane_waits_and_starts_on_a_later_pass() {
    let (mut rig, _) = awaiting(1);
    rig.queue(0, &["b"]);
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    let held = rig.machine.pool.claim(&crate::lane_pool::Want {
        project: "p0".into(),
        item: "b".into(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    });
    assert!(held.is_some());
    answer(&rig, "a", false, Some("Broken."));
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::AwaitingPlaytest);
    assert_eq!(rig.lp(0).playtest_fails.len(), 1);
    rig.machine.pool.release(
        held.unwrap(),
        &crate::lane_pool::Holder::Item {
            project: "p0".into(),
            item: "b".into(),
        },
    );
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).playtest_fails.is_empty());
}

#[tokio::test]
async fn an_answer_for_an_item_not_awaiting_a_playtest_changes_nothing() {
    let (mut rig, fake) = awaiting(2);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Implementing;
    answer(&rig, "a", true, None);
    answer(&rig, "nope", false, Some("x"));
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).playtest_fails.is_empty());
    assert!(fake.started.lock().unwrap().is_empty());
}

/// A store whose `tryouts_list` holds one waiting try-out (key "k1", item "7") and whose first `fail` answers to
/// `tryout_result` are 500s; every `tryout_result` it gets is kept.
async fn tryout_store(fail: usize) -> (String, Arc<Mutex<Vec<Value>>>) {
    let results = Arc::new(Mutex::new(Vec::new()));
    let seen = results.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("");
                let value = match op {
                    "tryouts_list" => {
                        let taken = seen.lock().unwrap().len() > fail;
                        let waiting = if taken {
                            json!([])
                        } else {
                            json!([{"key": "k1", "itemId": "7", "title": "T", "what": "W"}])
                        };
                        json!({"waiting": waiting, "groups": null})
                    }
                    "tryout_result" => {
                        let mut all = seen.lock().unwrap();
                        all.push(body["args"].clone());
                        if all.len() <= fail {
                            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                        }
                        Value::Null
                    }
                    _ => json!([]),
                };
                Json(json!({ "value": value })).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), results)
}

use axum::response::IntoResponse;

fn tryout_rig(base: &str) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    rig
}

#[tokio::test]
async fn a_pass_on_a_waiting_tryout_sends_one_passed_result() {
    let (base, results) = tryout_store(0).await;
    let mut rig = tryout_rig(&base);
    answer(&rig, "7", true, None);
    rig.machine.poll().await;
    rig.machine.poll().await;
    let sent = results.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0]["key"].as_str(), sent[0]["result"].as_str()),
        (Some("k1"), Some("passed"))
    );
    assert!(rig.lp(0).unanswered.is_empty());
}

#[tokio::test]
async fn a_fail_sends_a_problem_with_the_notes() {
    let (base, results) = tryout_store(0).await;
    let mut rig = tryout_rig(&base);
    answer(&rig, "k1", false, Some("Broken. Pictures: /tmp/a.png"));
    rig.machine.poll().await;
    let sent = results.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["result"], "problem");
    assert_eq!(sent[0]["notes"], "Broken. Pictures: /tmp/a.png");
}

#[tokio::test]
async fn an_answer_the_store_refuses_goes_again_and_only_once_after_it_is_taken() {
    let (base, results) = tryout_store(1).await;
    let mut rig = tryout_rig(&base);
    answer(&rig, "7", true, None);
    rig.machine.poll().await;
    assert_eq!(results.lock().unwrap().len(), 1);
    assert!(rig.lp(0).unanswered.contains_key("7"));
    rig.machine.pass().await;
    assert_eq!(results.lock().unwrap().len(), 2);
    assert!(rig.lp(0).unanswered.is_empty());
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(results.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_id_with_no_waiting_tryout_sends_nothing() {
    let (base, results) = tryout_store(0).await;
    let mut rig = tryout_rig(&base);
    answer(&rig, "nope", true, None);
    rig.machine.poll().await;
    assert!(results.lock().unwrap().is_empty());
    assert!(rig.lp(0).unanswered.is_empty());
}
