use serde_json::json;

use super::*;

fn row(args: Value) -> IntentRow {
    serde_json::from_value(json!({ "id": "in-1", "kind": "finish_requested", "status": "requested", "args": args }))
        .unwrap()
}

fn handler() -> (FinishHandler, mpsc::UnboundedReceiver<Finish>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (FinishHandler::new(tx), rx)
}

#[test]
fn it_takes_only_a_row_that_names_an_item() {
    let (h, _rx) = handler();
    let takes = |args| Handler::<()>::takes(&h, &row(args));
    assert!(takes(json!({ "itemId": "4" })));
    assert!(!takes(json!({})));
    assert!(!takes(json!({ "itemId": "" })));
    assert!(!takes(json!({ "itemId": 4 })));
}

#[tokio::test]
async fn the_loops_answer_ends_the_row() {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "itemId": "4" })), &());
    let answered = tokio::spawn(async move {
        let ask = rx.recv().await.unwrap();
        assert_eq!(ask.id, "4");
        ask.reply.send(Ok(json!({ "itemId": "4", "sha": "abc" }))).unwrap();
    });
    assert_eq!(running.await, Outcome::Done(json!({ "itemId": "4", "sha": "abc" })));
    answered.await.unwrap();
}

#[tokio::test]
async fn a_refusal_is_the_rows_reason() {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "itemId": "4" })), &());
    tokio::spawn(async move { rx.recv().await.unwrap().reply.send(Err("no".into())).unwrap() });
    assert_eq!(running.await, Outcome::Refused("no".into()));
}

#[tokio::test]
async fn a_loop_that_goes_away_leaves_the_row_for_the_store_to_open_again() {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "itemId": "4" })), &());
    tokio::spawn(async move { drop(rx.recv().await) });
    assert_eq!(running.await, Outcome::Abandoned);
    let (h, rx) = handler();
    drop(rx);
    assert_eq!(
        Handler::<()>::run(&h, row(json!({ "itemId": "4" })), &()).await,
        Outcome::Abandoned
    );
}
