use serde_json::json;

use super::*;

fn row() -> IntentRow {
    serde_json::from_value(json!({ "id": "in-1", "kind": "main_pr_requested", "status": "requested", "args": {} }))
        .unwrap()
}

fn handler() -> (MainPrHandler, mpsc::UnboundedReceiver<OpenMainPr>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (MainPrHandler::new(tx), rx)
}

async fn answering(answer: Answer) -> Outcome {
    let (h, mut rx) = handler();
    let running = Handler::<()>::run(&h, row(), &());
    tokio::spawn(async move { rx.recv().await.unwrap().reply.send(answer).unwrap() });
    running.await
}

#[test]
fn it_is_a_not_twice_kind_that_takes_every_row() {
    let (h, _rx) = handler();
    assert_eq!(Handler::<()>::kind(&h), "main_pr_requested");
    assert!(Handler::<()>::not_twice(&h));
    assert!(Handler::<()>::takes(&h, &row()));
}

#[tokio::test]
async fn the_url_ends_the_row_done_as_a_result() {
    let url = "https://github.com/o/r/pull/9";
    assert_eq!(answering(Ok(url.into())).await, Outcome::Done(json!({ "url": url })));
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
    let running = Handler::<()>::run(&h, row(), &());
    tokio::spawn(async move { drop(rx.recv().await) });
    assert_eq!(
        running.await,
        Outcome::Failed("This computer stopped before it answered; check, then ask again".into())
    );
    let (h, rx) = handler();
    drop(rx);
    assert_eq!(
        Handler::<()>::run(&h, row(), &()).await,
        Outcome::Failed("This computer stopped before it answered; check, then ask again".into())
    );
}

#[tokio::test]
async fn a_loop_that_goes_away_in_a_shutdown_leaves_the_row_for_the_store() {
    crate::process_tree::set_stopping(true);
    let (h, rx) = handler();
    drop(rx);
    let out = Handler::<()>::run(&h, row(), &()).await;
    crate::process_tree::reset_for_tests();
    assert_eq!(out, Outcome::Abandoned);
}

async fn drafting(answer: DraftAnswer) -> Outcome {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let h = MainPrDraftHandler::new(tx);
    let running = Handler::<()>::run(&h, row(), &());
    tokio::spawn(async move { rx.recv().await.unwrap().send(answer).unwrap() });
    running.await
}

#[tokio::test]
async fn a_main_pr_draft_row_is_a_read_that_ends_done_with_the_title_and_body() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let h = MainPrDraftHandler::new(tx);
    assert_eq!(Handler::<()>::kind(&h), "main_pr_draft");
    assert!(!Handler::<()>::not_twice(&h) && !Handler::<()>::starts_work(&h) && !Handler::<()>::runs_session(&h));
    let draft = crate::team_branch::main_pr::Draft {
        title: "dev: 2 slices".into(),
        body: "- a\n- b".into(),
    };
    assert_eq!(
        drafting(Ok(draft)).await,
        Outcome::Done(json!({ "title": "dev: 2 slices", "body": "- a\n- b" }))
    );
    assert_eq!(
        drafting(Err(Why::Refused("no dev branch".into()))).await,
        Outcome::Refused("no dev branch".into())
    );
    assert_eq!(
        drafting(Err(Why::Failed("boom".into()))).await,
        Outcome::Failed("boom".into())
    );
}
