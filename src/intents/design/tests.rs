use serde_json::json;

use super::*;

fn row(kind: &str, args: serde_json::Value) -> IntentRow {
    IntentRow {
        id: "in-1".into(),
        kind: kind.into(),
        args,
        issued_by: Some("ana".into()),
        status: "claimed".into(),
    }
}

/// Runs `row` with a loop that answers the press it is handed with `answer`; returns the outcome and the press.
async fn answered(
    handler: DesignHandler,
    row: IntentRow,
    answer: Result<(), String>,
) -> (Outcome, Option<(String, Option<String>, bool)>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Press>();
    let handler = DesignHandler { presses: tx, ..handler };
    let running = Handler::<()>::run(&handler, row, &());
    let looping = async {
        let press = rx.recv().await?;
        let seen = (press.draft.clone(), press.note.clone(), press.draw);
        let _ = press.reply.send(answer);
        Some(seen)
    };
    let (outcome, seen) = tokio::join!(running, looping);
    (outcome, seen)
}

fn handler(draw: bool) -> DesignHandler {
    let (presses, _) = mpsc::unbounded_channel();
    DesignHandler { draw, presses }
}

#[test]
fn a_skip_starts_no_work_and_a_draw_starts_work_and_runs_a_session() {
    let (skip, draw) = (handler(false), handler(true));
    assert_eq!(Handler::<()>::kind(&skip), "design_skip_requested");
    assert_eq!(Handler::<()>::kind(&draw), "design_draw_requested");
    assert!(!Handler::<()>::starts_work(&skip) && !Handler::<()>::runs_session(&skip));
    assert!(Handler::<()>::starts_work(&draw) && Handler::<()>::runs_session(&draw));
    assert!(
        !Handler::<()>::not_twice(&draw),
        "a press can be run again after a restart"
    );
}

#[tokio::test]
async fn a_skip_hands_the_draft_over_and_ends_done_with_no_result() {
    let row = row("design_skip_requested", json!({ "draftId": " d1 ", "note": "ignored" }));
    let (outcome, seen) = answered(handler(false), row, Ok(())).await;
    assert_eq!(outcome, Outcome::Done(serde_json::Value::Null));
    assert_eq!(seen, Some(("d1".into(), None, false)), "a skip carries no note");
}

#[tokio::test]
async fn a_draw_carries_the_persons_note_cut_to_2000_characters_and_a_blank_one_is_none() {
    let long = row(
        "design_draw_requested",
        json!({ "draftId": "d1", "note": format!("  {}", "x".repeat(2500)) }),
    );
    let (outcome, seen) = answered(handler(true), long, Ok(())).await;
    assert_eq!(outcome, Outcome::Done(serde_json::Value::Null));
    let (draft, note, draw) = seen.unwrap();
    assert_eq!(
        (draft.as_str(), note.unwrap().chars().count(), draw),
        ("d1", 2000, true)
    );
    let blank = row("design_draw_requested", json!({ "draftId": "d1", "note": "   " }));
    assert_eq!(
        answered(handler(true), blank, Ok(())).await.1,
        Some(("d1".into(), None, true))
    );
}

#[tokio::test]
async fn a_press_the_loop_refuses_ends_the_row_refused_in_its_words_cut_to_280() {
    let row = row("design_draw_requested", json!({ "draftId": "d1" }));
    let (outcome, _) = answered(
        handler(true),
        row.clone(),
        Err("This plan has no design to review.".into()),
    )
    .await;
    assert_eq!(outcome, Outcome::Refused("This plan has no design to review.".into()));
    let (outcome, _) = answered(handler(true), row, Err("w".repeat(400))).await;
    assert_eq!(outcome, Outcome::Refused("w".repeat(280)));
}

#[tokio::test]
async fn a_row_with_no_usable_draft_id_is_refused_without_asking_the_loop() {
    for args in [
        json!({}),
        json!({ "draftId": "" }),
        json!({ "draftId": "../x" }),
        json!({ "draftId": 7 }),
    ] {
        let (tx, mut rx) = mpsc::unbounded_channel::<Press>();
        let h = DesignHandler {
            draw: true,
            presses: tx,
        };
        let outcome = Handler::<()>::run(&h, row("design_draw_requested", args), &()).await;
        assert_eq!(
            outcome,
            Outcome::Refused("design_draw_requested needs the draft's id.".into())
        );
        assert!(rx.try_recv().is_err(), "the loop was never asked");
    }
}

#[tokio::test]
async fn a_loop_that_drops_the_reply_leaves_the_row_to_the_stale_claim_or_fails_it() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Press>();
    let h = DesignHandler {
        draw: false,
        presses: tx,
    };
    let running = Handler::<()>::run(&h, row("design_skip_requested", json!({ "draftId": "d1" })), &());
    let looping = async { drop(rx.recv().await) };
    let (outcome, ()) = tokio::join!(running, looping);
    assert_eq!(outcome, crate::intents::dropped());
}
