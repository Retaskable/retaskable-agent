//! A person's problem with a try-out this computer sent reopens the item it built (`driver/reopening.rs`).
use super::super::*;
use crate::store::BuiltTryout;

/// A store that answers the try-out's `tryout_taken`, and `reopen` with the item under a lease (or, when `reopens` is false,
/// `{ reopened: false }`), recording every op.
async fn problem_store(reopens: bool) -> (String, Asked) {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let quiet = ["settings_get", "pause_get", "facts_get", "details_get", "details_report"];
                if quiet.contains(&op.as_str()) {
                    return Json(json!({ "value": if op == "pause_get" { json!({ "paused": null, "log": [] }) } else { Value::Null } }));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "tryout_taken" => json!({ "taken": true, "itemId": "a", "title": "A", "what": "the chart", "steps": ["Open it"], "prUrl": "https://github.com/o/p/pull/7", "mergedAt": "m1", "by": "carol" }),
                    "reopen" if reopens => json!({ "reopened": true, "item": item("a", "claimed"), "token": "lease-9" }),
                    "reopen" => json!({ "reopened": false, "status": "building" }),
                    "chore_trigger" => json!({ "queued": true }),
                    "health" => json!({ "tasks": [], "stop": [], "free": 1, "allowed": 2, "tuning": crate::store_tuning::StoreTuning::test_json() }),
                    _ => json!([]),
                };
                Json(json!({ "value": value }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn ops(asked: &Asked, name: &str) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == name)
        .map(|(_, a)| a.clone())
        .collect()
}

fn built(rig: &mut Rig) {
    let lp = rig.lp(0);
    lp.store.set_tryouts_built(&[BuiltTryout {
        key: "a@m1".into(),
        session_id: Some("s1".into()),
        lane: Some(1),
    }]);
}

#[tokio::test]
async fn a_problem_with_a_tryout_this_computer_sent_takes_it_and_reopens_the_item_under_a_new_lease() {
    let (base, asked) = problem_store(true).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    built(&mut rig);
    rig.machine.links.work.problem("p0", "a@m1", "It crashes");
    rig.machine.pass().await;
    rig.machine.pass().await;
    settle().await;
    // Like a claimed item, it starts a session of its own.
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    let taken = ops(&asked, "tryout_taken");
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0]["key"], json!("a@m1"));
    let reopen = &ops(&asked, "reopen")[0];
    assert_eq!(
        (
            reopen["id"].as_str(),
            reopen["notes"].as_str(),
            reopen["by"].as_str(),
            reopen["what"].as_str()
        ),
        (Some("a"), Some("It crashes"), Some("carol"), Some("the chart"))
    );
    assert_eq!(reopen["steps"], json!(["Open it"]));
    assert!(reopen["agent"]["machine"].is_string());
    let lp = rig.lp(0);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-9"));
    assert!(lp.runs.contains_key("a"));
    assert!(lp.store.tryouts_built().is_empty(), "the problem is taken once");
    assert!(ops(&asked, "chore_trigger").is_empty());
}

#[tokio::test]
async fn an_item_the_store_keeps_as_it_is_gets_a_playtest_fix_asked_for_instead() {
    let (base, asked) = problem_store(false).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    built(&mut rig);
    rig.machine.links.work.problem("p0", "a@m1", "It crashes");
    rig.machine.pass().await;
    let fix = &ops(&asked, "chore_trigger")[0];
    assert_eq!(fix["trigger"], json!("playtest_fix"));
    assert_eq!(
        (
            fix["data"]["key"].as_str(),
            fix["data"]["itemId"].as_str(),
            fix["data"]["notes"].as_str()
        ),
        (Some("a@m1"), Some("a"), Some("It crashes"))
    );
    assert!(rig.lp(0).leases.is_empty() && rig.lp(0).runs.is_empty());
}

#[tokio::test]
async fn a_problem_with_a_tryout_this_computer_did_not_send_is_left_to_the_store() {
    let (base, asked) = problem_store(true).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.links.work.problem("p0", "other@m", "It crashes");
    rig.machine.pass().await;
    assert!(ops(&asked, "tryout_taken").is_empty() && ops(&asked, "reopen").is_empty());
}
