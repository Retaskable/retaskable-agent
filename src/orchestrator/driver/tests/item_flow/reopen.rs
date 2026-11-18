//! A person's problem with a try-out this computer sent reopens the item it built (`driver/reopening.rs`).
use super::super::*;
use crate::store::BuiltTryout;

/// A store that answers the try-out's `tryout_taken`, and `reopen` with `reply` (the item under a lease, or `{ reopened: false,
/// fix }`), recording every op.
async fn problem_store(reply: Value) -> (String, Asked) {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, reply) = (seen.clone(), reply.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                let quiet = ["settings_get", "pause_get", "facts_get", "details_get", "details_report"];
                if quiet.contains(&op.as_str()) {
                    return Json(json!({ "value": if op == "pause_get" { json!({ "paused": null, "log": [] }) } else { Value::Null } }));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "tryout_taken" => json!({ "taken": true, "itemId": "a", "title": "A", "what": "the chart", "steps": ["Open it"], "prUrl": "https://github.com/o/p/pull/7", "mergedAt": "m1", "by": "carol" }),
                    "reopen" => reply.clone(),
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

/// `reopen`'s answer when the item is handed back under a lease.
fn reopened() -> Value {
    json!({ "reopened": true, "item": item("a", "claimed"), "token": "lease-9" })
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
    let (base, asked) = problem_store(reopened()).await;
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
    assert_eq!(reopen["key"], json!("a@m1"));
    let lp = rig.lp(0);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-9"));
    assert!(lp.runs.contains_key("a"));
    assert!(lp.store.tryouts_built().is_empty(), "the problem is taken once");
    assert!(ops(&asked, "chore_trigger").is_empty());
}

#[tokio::test]
async fn an_item_the_store_keeps_is_given_a_playtest_fix_by_the_store_and_the_agent_sends_no_trigger() {
    for fix in ["queued", "open", "taken"] {
        let (base, asked) = problem_store(json!({ "reopened": false, "status": "building", "fix": fix })).await;
        let mut rig = rig(2, &[&base]);
        rig.go_live();
        built(&mut rig);
        rig.machine.links.work.problem("p0", "a@m1", "It crashes");
        rig.machine.pass().await;
        let reopen = &ops(&asked, "reopen")[0];
        assert_eq!(
            (
                reopen["key"].as_str(),
                reopen["title"].as_str(),
                reopen["prUrl"].as_str()
            ),
            (Some("a@m1"), Some("A"), Some("https://github.com/o/p/pull/7")),
            "the store gets what it needs to make the fix"
        );
        assert!(
            ops(&asked, "chore_trigger").is_empty(),
            "{fix}: the store made it, the agent asks for none"
        );
        assert!(
            rig.lp(0).leases.is_empty() && rig.lp(0).runs.is_empty(),
            "{fix}: nothing is held here"
        );
        assert!(rig.lp(0).store.tryouts_built().is_empty(), "the problem is taken once");
    }
}

#[tokio::test]
async fn an_older_store_that_made_no_fix_leaves_nothing_for_the_agent_to_trigger() {
    let (base, asked) = problem_store(json!({ "reopened": false, "status": "building" })).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    built(&mut rig);
    rig.machine.links.work.problem("p0", "a@m1", "It crashes");
    rig.machine.pass().await;
    assert!(ops(&asked, "chore_trigger").is_empty());
    assert!(rig.lp(0).leases.is_empty() && rig.lp(0).runs.is_empty());
}

#[tokio::test]
async fn a_problem_with_a_tryout_this_computer_did_not_send_is_left_to_the_store() {
    let (base, asked) = problem_store(reopened()).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.links.work.problem("p0", "other@m", "It crashes");
    rig.machine.pass().await;
    assert!(ops(&asked, "tryout_taken").is_empty() && ops(&asked, "reopen").is_empty());
}
