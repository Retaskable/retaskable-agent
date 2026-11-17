//! `go_live` and `confirm`, the pushed commands the loop answers by asking the web or the store one thing
//! (`read_kinds::live`, run by `read_pass`).
use axum::Router;
use axum::routing::get;

use super::super::asks::{Listed, ops_named, store_failing};
use super::*;
use crate::view::ReadCommand;

async fn asked_of(kind: &str, args: Value) -> (Value, Asked, Listed) {
    let (base, asked, listed) = store_failing("").await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.wanted = true;
    let items = rig.machine.links.item_commands.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    items.push_read(ReadCommand {
        project: "p0".into(),
        command_id: "c1".into(),
        kind: kind.into(),
        args,
    });
    rig.machine.pass().await;
    let answer = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    (answer, asked, listed)
}

#[tokio::test]
async fn confirm_is_acked_with_no_result_after_the_store_processes_the_item() {
    let (answer, asked, _) = asked_of("confirm", json!({ "itemId": "a-1" })).await;
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(ops_named(&asked, "process"), [json!({ "id": "a-1" })]);
}

#[tokio::test]
async fn go_live_answers_the_address_it_checked_and_refuses_one_that_does_not_answer() {
    let app = Router::new().route("/ok", get(|| async { "hi" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (answer, ..) = asked_of("go_live", json!({ "url": format!("{base}/ok") })).await;
    assert_eq!(
        answer,
        json!({ "type": "ack", "id": "c1", "result": { "url": format!("{base}/ok") } })
    );
    let (answer, ..) = asked_of("go_live", json!({ "url": format!("{base}/nope") })).await;
    assert_eq!(
        (answer["type"].as_str(), answer["id"].as_str()),
        (Some("nack"), Some("c1"))
    );
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("answered 404. If the provider is still building it")
    );
}
