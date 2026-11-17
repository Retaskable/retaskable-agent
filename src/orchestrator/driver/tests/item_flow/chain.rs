//! The whole chain of a leased item through the real loop, with a fake store, a fake engine and the steps' ends scripted: the
//! store sends the task, the item is claimed by name, built, checked, reviewed (a try-out deferred until after the merge), parked
//! for CI, claimed back, merged on the store's turn, completed and its try-out sent. Each step's own decisions are
//! `orchestrator/flow/`'s tests; what is tested here is that the loop carries one into the next and tells the store each in order.
use super::super::*;
use crate::orchestrator::steps::{MergeEnd, state_name};
use crate::store::{FlowRecord, TryOut};

const PR: &str = "https://github.com/o/p/pull/7";

async fn store(tasks: Vec<Value>) -> (String, Asked, Details) {
    use axum::http::StatusCode;
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let details: Details = Default::default();
    let kept = details.clone();
    let tasks = Arc::new(Mutex::new(tasks));
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, details, tasks) = (seen.clone(), details.clone(), tasks.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                if let Some(answer) = details_answer(&details, &op, &body["args"]) {
                    return answer;
                }
                if matches!(op.as_str(), "settings_get" | "pause_get" | "facts_get") {
                    let value = match op.as_str() {
                        "pause_get" => json!({ "paused": null, "log": [] }),
                        "facts_get" => json!({}),
                        _ => Value::Null,
                    };
                    return (StatusCode::OK, Json(json!({ "value": value })));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "health" => json!({ "tasks": tasks.lock().unwrap().clone(), "stop": [], "free": 1, "allowed": 2 }),
                    "claim" => {
                        tasks.lock().unwrap().clear();
                        json!({ "item": item("a", "claimed"), "token": "lease-a" })
                    }
                    "task_end" => json!({ "held": true }),
                    "complete" => json!({ "built": false, "status": "todo" }),
                    "merge_next" => json!({ "turn": { "token": "turn-1", "entry": { "item": "a" } } }),
                    "merge_beat" | "merge_result" => json!({ "held": true }),
                    "merge_open" | "merge_enqueue" | "merge_withdraw" | "park" | "release" | "report"
                    | "tryout_add" => json!({}),
                    _ => json!([item("a", "claimed")]),
                };
                (StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked, kept)
}

fn done(state: ItemState, flow: Option<FlowRecord>) -> StepDone {
    let mut run = ItemRun::new();
    run.state = state;
    run.approved_at_head = state == ItemState::AwaitingCi;
    StepDone {
        run,
        flow,
        ..Default::default()
    }
}

fn ops_in_order(asked: &Asked) -> Vec<String> {
    super::super::flow::calls(asked)
}

#[tokio::test]
async fn an_item_goes_from_the_stores_task_to_merged_and_completed_with_its_tryout_sent_and_the_task_ended() {
    let task = json!({ "id": "task-a", "subject": "a", "tier": "smart", "prompt": "" });
    let (base, asked, details) = store(vec![task]).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    // The engine: one implementer session that finishes.
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::Implementer(ImplementerOutcome::Finished));
    let pending = TryOut {
        what: "the chart".into(),
        steps: vec!["Open it".into()],
    };
    let reviewed = FlowRecord {
        pr_url: Some(PR.into()),
        pr_number: Some(7),
        approved_sha: Some("abc".into()),
        pending_playtest: Some(pending.clone()),
        ..Default::default()
    };
    let mut merged = done(ItemState::Merged, None);
    merged.merge = Some(MergeEnd::Merged {
        commit: Some("deadbeef".into()),
    });
    // What each state's step hands back, in the order the chain reaches them.
    let mut script: std::collections::BTreeMap<&str, StepDone> = [
        (
            "checking",
            done(
                ItemState::Reviewing,
                Some(FlowRecord {
                    pr_url: Some(PR.into()),
                    pr_number: Some(7),
                    ..Default::default()
                }),
            ),
        ),
        ("reviewing", done(ItemState::AwaitingCi, Some(reviewed))),
        ("awaiting_ci", done(ItemState::Merging, None)),
        ("merging", merged),
    ]
    .into();
    let mut seen_states = Vec::new();
    let mut wrote = false;
    for _ in 0..20 {
        let state = rig.lp(0).runs.get("a").map(|r| state_name(r.state));
        if let Some(state) = state
            && fake.done.lock().unwrap().is_none()
            && let Some(next) = script.remove(state)
        {
            seen_states.push(state);
            *fake.done.lock().unwrap() = Some(next);
        }
        rig.machine.pass().await;
        settle().await;
        wrote |= (details.lock().unwrap().get("a")).is_some_and(|(_, d)| d.contains_key("pendingPlaytest"));
        if script.is_empty() && rig.lp(0).outbox.is_empty() && !rig.lp(0).runs.contains_key("a") {
            break;
        }
    }
    assert!(wrote, "the deferred try-out was in the item's details before it merged");
    assert_eq!(seen_states, ["checking", "reviewing", "awaiting_ci", "merging"]);
    let calls = ops_in_order(&asked);
    let at = |op: &str| {
        calls
            .iter()
            .position(|c| c == op)
            .unwrap_or_else(|| panic!("no {op} in {calls:?}"))
    };
    let last = |op: &str| {
        calls
            .iter()
            .rposition(|c| c == op)
            .unwrap_or_else(|| panic!("no {op} in {calls:?}"))
    };
    // The store's task is claimed by name, the item built, handed to CI (the item is parked, and the task ends), claimed
    // back, merged on the store's turn, completed, and its try-out sent.
    assert!(at("health") < at("claim") && at("claim") < at("task_end"), "{calls:?}");
    assert!(
        at("claim") < at("park") && at("park") < last("claim") && at("task_end") < last("claim"),
        "{calls:?}"
    );
    assert!(
        last("claim") < at("merge_enqueue")
            && at("merge_enqueue") < at("merge_next")
            && at("merge_next") < at("merge_beat")
            && at("merge_beat") < at("merge_result")
            && at("merge_result") < at("complete")
            && at("complete") < at("tryout_add"),
        "{calls:?}"
    );
    let sent = |op: &str| -> Vec<Value> {
        asked
            .lock()
            .unwrap()
            .iter()
            .filter(|(o, _)| o == op)
            .map(|(_, a)| a.clone())
            .collect()
    };
    assert_eq!(sent("task_end"), [json!({ "id": "task-a", "ok": true })]);
    assert_eq!(sent("park")[0]["for"], json!("ci"));
    assert_eq!(sent("merge_result")[0]["outcome"], json!("merged"));
    let complete = &sent("complete")[0];
    assert_eq!(
        (complete["id"].as_str(), complete["mergeCommit"].as_str()),
        (Some("a"), None)
    );
    let add = &sent("tryout_add")[0];
    assert_eq!(
        (add["itemId"].as_str(), add["what"].as_str(), add["sha"].as_str()),
        (Some("a"), Some("the chart"), Some("deadbeef"))
    );
    // Nothing is left: no run, no lease, no completion owed, and the deferred try-out is no longer in the item's details.
    let lp = rig.lp(0);
    assert!(lp.runs.is_empty() && lp.leases.is_empty() && lp.outbox.is_empty() && lp.parked.is_empty());
    let kept = details.lock().unwrap();
    assert!(
        kept.get("a").is_none_or(|(_, d)| !d.contains_key("pendingPlaytest")),
        "{kept:?}"
    );
}
