//! `claim_next` answering `{ waitingFor }` behind the `claim-next-waiting` flag: the claim path waits for
//! `work_available`, logs the missing tags once per change, and records no `store_error` (`driver/claim.rs`).
use super::*;

/// A store whose `claim_next` answers the value in the returned cell; every op is recorded.
async fn stub(first: Value) -> (String, Asked, Arc<Mutex<Value>>) {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let answer = Arc::new(Mutex::new(first));
    let (seen, now) = (asked.clone(), answer.clone());
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, now) = (seen.clone(), now.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = if op == "claim_next" {
                    now.lock().unwrap().clone()
                } else {
                    json!([])
                };
                Json(json!({ "value": value }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked, answer)
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim_next").count()
}

fn waiting_events(rig: &mut Rig) -> Vec<Value> {
    rig.lp(0)
        .events()
        .recent()
        .iter()
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "claim" && e["answer"].get("waitingFor").is_some())
        .collect()
}

#[tokio::test]
async fn a_wait_asks_nothing_until_a_wake_then_asks_once_and_logs_once_per_change() {
    let (base, asked, answer) = stub(json!({ "waitingFor": ["gpu"] })).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["claim-next-waiting"]));
    let fake = rig.go_live();

    rig.machine.pass().await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1, "no second ask while waiting");
    assert!(fake.started.lock().unwrap().is_empty());
    let kinds: Vec<_> = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .map(|e| e.kind().to_string())
        .collect();
    assert!(
        !kinds.iter().any(|k| k == "store_error" || k == "repeat_error"),
        "{kinds:?}"
    );
    assert_eq!(waiting_events(&mut rig).len(), 1);

    let id = rig.machine.projects[0].id.clone();
    rig.machine.links.work.wake(&id);
    rig.machine.links.work.wake(&id);
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 2, "one wake, one more ask");
    assert_eq!(waiting_events(&mut rig).len(), 1, "the same list isn't logged again");

    *answer.lock().unwrap() = json!({ "waitingFor": ["gpu", "mac"] });
    rig.machine.links.work.wake(&id);
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 3);
    let logged = waiting_events(&mut rig);
    assert_eq!(logged.len(), 2, "a changed list is logged");
    assert_eq!(logged[1]["answer"]["waitingFor"], json!(["gpu", "mac"]));
}

#[tokio::test]
async fn with_the_flag_off_a_wait_is_a_store_error_as_before() {
    let (base, _asked, _) = stub(json!({ "waitingFor": ["gpu"] })).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.pass().await;
    let kinds: Vec<_> = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .map(|e| e.kind().to_string())
        .collect();
    assert!(kinds.iter().any(|k| k == "store_error"), "{kinds:?}");
}
