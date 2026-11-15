use serde_json::{Value, json};

use super::*;

fn row(args: Value) -> IntentRow {
    serde_json::from_value(json!({ "id": "in-1", "kind": "merge_requested", "status": "requested", "args": args }))
        .unwrap()
}

fn handler() -> (MergeHandler, mpsc::UnboundedReceiver<Merge>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (MergeHandler::new(tx), rx)
}

#[test]
fn it_is_a_not_twice_kind() {
    let (h, _rx) = handler();
    assert!(Handler::<()>::not_twice(&h));
}

#[test]
fn it_leaves_a_row_naming_only_the_pull_request_into_main() {
    let (h, _rx) = handler();
    let takes = |args| Handler::<()>::takes(&h, &row(args));
    assert!(takes(json!({ "itemId": "4" })));
    assert!(takes(json!({ "draftId": "draft-1" })));
    assert!(!takes(json!({ "ship": true })));
    // Naming none or several is refused here, so it is taken.
    assert!(takes(json!({})));
    assert!(takes(json!({ "itemId": "4", "draftId": "draft-1" })));
    assert!(takes(json!({ "draftId": "draft-1", "ship": true })));
    // An empty id or a `ship` that isn't true names nothing.
    assert!(takes(json!({ "itemId": "" })));
    assert!(!takes(json!({ "itemId": "", "ship": true })));
    assert!(takes(json!({ "ship": false })));
}

#[tokio::test]
async fn a_row_naming_more_than_one_is_refused_with_the_ts_words_and_asks_nothing() {
    let (h, mut rx) = handler();
    for args in [
        json!({}),
        json!({ "itemId": "4", "draftId": "d" }),
        json!({ "itemId": "4", "ship": true }),
    ] {
        let out = Handler::<()>::run(&h, row(args), &()).await;
        assert_eq!(
            out,
            Outcome::Refused("Name one item, one draft, or the pull request into main to merge.".into())
        );
    }
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn the_loops_answer_ends_the_row_done_with_no_result() {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "itemId": "4" })), &());
    let answered = tokio::spawn(async move {
        let ask = rx.recv().await.unwrap();
        assert_eq!((ask.id.as_str(), ask.draft), ("4", false));
        ask.reply.send(Ok(())).unwrap();
    });
    assert_eq!(running.await, Outcome::Done(Value::Null));
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
async fn a_loop_that_goes_away_leaves_the_row_for_the_store() {
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

#[tokio::test]
async fn a_draft_row_asks_the_loop_for_that_draft() {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "draftId": "draft-1" })), &());
    let answered = tokio::spawn(async move {
        let ask = rx.recv().await.unwrap();
        assert_eq!((ask.id.as_str(), ask.draft), ("draft-1", true));
        ask.reply.send(Err("no".into())).unwrap();
    });
    assert_eq!(running.await, Outcome::Refused("no".into()));
    answered.await.unwrap();
}
