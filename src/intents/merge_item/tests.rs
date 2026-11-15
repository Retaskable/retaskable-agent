use serde_json::{Value, json};

use super::*;

fn row(args: Value) -> IntentRow {
    serde_json::from_value(json!({ "id": "in-1", "kind": "merge_requested", "status": "requested", "args": args }))
        .unwrap()
}

fn handler() -> (MergeHandler, mpsc::UnboundedReceiver<Merge>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (ships, _) = mpsc::unbounded_channel();
    (MergeHandler::new(tx, ships), rx)
}

fn ship_handler() -> (
    MergeHandler,
    mpsc::UnboundedReceiver<Ship>,
    mpsc::UnboundedReceiver<Merge>,
) {
    let (tx, items) = mpsc::unbounded_channel();
    let (ships, rx) = mpsc::unbounded_channel();
    (MergeHandler::new(tx, ships), rx, items)
}

#[test]
fn it_is_a_not_twice_kind() {
    let (h, _rx) = handler();
    assert!(Handler::<()>::not_twice(&h));
}

#[test]
fn it_takes_every_row_of_its_kind() {
    let (h, _rx) = handler();
    for args in [
        json!({ "itemId": "4" }),
        json!({ "draftId": "draft-1" }),
        json!({ "ship": true }),
        json!({}),
        json!({ "itemId": "4", "ship": true }),
    ] {
        assert!(Handler::<()>::takes(&h, &row(args)));
    }
}

#[tokio::test]
async fn a_row_naming_more_than_one_is_refused_with_the_ts_words_and_asks_nothing() {
    let (h, mut rx) = handler();
    for args in [
        json!({}),
        json!({ "itemId": "4", "draftId": "d" }),
        json!({ "itemId": "4", "ship": true }),
        json!({ "draftId": "d", "ship": true }),
        json!({ "ship": false }),
        json!({ "itemId": "", "ship": false }),
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
async fn a_row_naming_more_than_one_asks_for_no_ship_either() {
    let (h, mut ships, mut items) = ship_handler();
    for args in [
        json!({ "itemId": "4", "ship": true }),
        json!({ "draftId": "d", "ship": true }),
    ] {
        let out = Handler::<()>::run(&h, row(args), &()).await;
        assert!(matches!(out, Outcome::Refused(_)));
    }
    assert!(ships.try_recv().is_err() && items.try_recv().is_err());
}

/// What the row ends as, for the loop's `answer` to a ship-only row (and that no item was asked for).
async fn ship_ends(answer: ShipAnswer) -> Outcome {
    let (h, mut rx, mut items) = ship_handler();
    let running = Handler::<()>::run(&h, row(json!({ "ship": true })), &());
    let answered = tokio::spawn(async move { rx.recv().await.unwrap().reply.send(answer).unwrap() });
    let out = running.await;
    answered.await.unwrap();
    assert!(items.try_recv().is_err());
    out
}

#[tokio::test]
async fn a_ship_ends_done_with_no_result_unless_the_loop_has_something_to_say() {
    assert_eq!(ship_ends(Ok(None)).await, Outcome::Done(Value::Null));
    assert_eq!(
        ship_ends(Ok(Some("dev was left as it is.".into()))).await,
        Outcome::Done(json!({ "says": "dev was left as it is." }))
    );
    assert_eq!(
        ship_ends(Err(Why::Refused("no".into()))).await,
        Outcome::Refused("no".into())
    );
    assert_eq!(
        ship_ends(Err(Why::Failed("broke".into()))).await,
        Outcome::Failed("broke".into())
    );
}

#[tokio::test]
async fn a_ship_whose_loop_goes_away_leaves_the_row_for_the_store() {
    let (h, mut rx, _items) = ship_handler();
    let running = Handler::<()>::run(&h, row(json!({ "ship": true })), &());
    tokio::spawn(async move { drop(rx.recv().await) });
    assert_eq!(running.await, Outcome::Abandoned);
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
