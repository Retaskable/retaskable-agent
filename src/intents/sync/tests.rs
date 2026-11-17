use serde_json::{Value, json};

use super::*;

fn row(args: Value) -> IntentRow {
    serde_json::from_value(json!({ "id": "in-1", "kind": "sync_requested", "status": "requested", "args": args }))
        .unwrap()
}

fn handler() -> (SyncHandler, mpsc::UnboundedReceiver<SyncNow>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (SyncHandler::new(tx), rx)
}

async fn answering(answer: Answer) -> Outcome {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "dev": true })), &());
    tokio::spawn(async move { rx.recv().await.unwrap().reply.send(answer).unwrap() });
    running.await
}

#[test]
fn it_is_a_not_twice_kind_that_takes_every_row() {
    let (h, _rx) = handler();
    assert_eq!(Handler::<()>::kind(&h), "sync_requested");
    assert!(Handler::<()>::not_twice(&h));
    assert!(Handler::<()>::takes(&h, &row(json!({ "dev": true }))));
    assert!(Handler::<()>::takes(&h, &row(json!({}))));
}

#[tokio::test]
async fn a_row_without_dev_is_refused_without_asking_the_loop() {
    for args in [json!({}), json!({ "dev": false }), json!({ "dev": "yes" })] {
        let (h, mut rx) = handler();
        let outcome = Handler::<()>::run(&h, row(args), &()).await;
        assert_eq!(
            outcome,
            Outcome::Refused("This project's code isn't synced anywhere else.".into())
        );
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn the_chore_id_ends_the_row_done_as_a_result() {
    assert_eq!(
        answering(Ok("chore-sync-main-abc1234".into())).await,
        Outcome::Done(json!({ "id": "chore-sync-main-abc1234" }))
    );
}

#[tokio::test]
async fn a_refusal_is_refused_and_a_breakage_is_failed() {
    assert_eq!(
        answering(Err(Why::Refused("no".into()))).await,
        Outcome::Refused("no".into())
    );
    assert_eq!(
        answering(Err(Why::Failed("boom".into()))).await,
        Outcome::Failed("boom".into())
    );
}

#[tokio::test]
async fn a_loop_that_goes_away_fails_the_row_in_words() {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(json!({ "dev": true })), &());
    tokio::spawn(async move { drop(rx.recv().await) });
    assert_eq!(
        running.await,
        Outcome::Failed("This computer stopped before it answered; check, then ask again".into())
    );
    let (h, rx) = handler();
    drop(rx);
    assert_eq!(
        Handler::<()>::run(&h, row(json!({ "dev": true })), &()).await,
        Outcome::Failed("This computer stopped before it answered; check, then ask again".into())
    );
}

#[tokio::test]
async fn a_loop_that_goes_away_in_a_shutdown_leaves_the_row_for_the_store() {
    crate::process_tree::set_stopping(true);
    let (h, rx) = handler();
    drop(rx);
    let out = Handler::<()>::run(&h, row(json!({ "dev": true })), &()).await;
    crate::process_tree::reset_for_tests();
    assert_eq!(out, Outcome::Abandoned);
}
