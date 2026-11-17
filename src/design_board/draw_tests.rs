//! The drawing and the design review's presses (TS `designBoard.test.ts`, `drafts.test.ts`'s `skipDesign`).
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::post};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::act::{Acting, draw_design, skip_design};
use super::draw::{Drawing, draw_design_board};
use super::error::DesignError;
use super::session::Idea;
use super::socket::{Timing, fake::Board};
use crate::backlog::{ClaimNextAgent, Client};

type Seen = Arc<Mutex<Vec<Value>>>;

/// A fake store: `board_ticket` gives a ticket, `draft_take` the draft (with its lease), the rest nothing.
async fn store(draft: Value) -> (Client, Seen) {
    let seen: Seen = Arc::default();
    async fn ops(
        State((seen, draft)): State<(Seen, Value)>,
        Json(body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        seen.lock().unwrap().push(body.clone());
        let value = match body["op"].as_str() {
            Some("board_ticket") => json!({"ticket": "t1"}),
            Some("draft_take") => json!({"token": "lease-1", "draft": draft}),
            _ => Value::Null,
        };
        (axum::http::StatusCode::OK, Json(json!({"value": value})))
    }
    let app = Router::new()
        .route("/v1/repos/{id}/ops", post(ops))
        .with_state((seen.clone(), draft));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (Client::new(base), seen)
}

fn ops_named(seen: &Seen, op: &str) -> Vec<Value> {
    seen.lock().unwrap().iter().filter(|b| b["op"] == op).cloned().collect()
}

fn fast() -> Timing {
    Timing {
        send_gap: Duration::ZERO,
        open: Duration::from_secs(5),
        write: Duration::from_secs(5),
    }
}

fn layout() -> Value {
    json!({"page": "board", "region": "header",
        "variations": [{"name": "A: button", "pieces": [{"element": "button", "label": "Snooze"}]}]})
}

fn sticky() -> Value {
    json!({"id": "note-1", "kind": "sticky", "x": 0, "y": 0, "w": 200, "h": 200, "text": "Keep it to one tap", "colour": 0})
}

fn agent() -> ClaimNextAgent {
    ClaimNextAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: vec![],
        week: None,
    }
}

type Asked = Arc<Mutex<Vec<String>>>;

/// A session that answers `answer` and keeps the prompts it was asked.
fn session(
    answer: Result<Value, DesignError>,
) -> (
    impl Fn(String) -> BoxFuture<'static, Result<Value, DesignError>> + Send + Sync,
    Asked,
) {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    let f = move |prompt: String| -> BoxFuture<'static, Result<Value, DesignError>> {
        seen.lock().unwrap().push(prompt);
        let answer = answer.clone();
        Box::pin(async move { answer })
    };
    (f, asked)
}

fn idea<'a>(change: Option<&'a str>) -> Idea<'a> {
    Idea {
        title: "Snooze an alert",
        request: "Let people snooze",
        summary: Some("A snooze button"),
        change,
    }
}

#[tokio::test]
async fn the_layout_is_written_to_the_ideas_board_as_the_chats_own_things_never_touching_a_persons_note() {
    let (client, seen) = store(json!({})).await;
    let board = Board::new(vec![sticky()]);
    let (ask, asked) = session(Ok(layout()));
    let d = Drawing {
        client: &client,
        repo: 7,
        token: "tok",
        transport: &board,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    draw_design_board(&d, "draft-abc", &idea(None)).await.unwrap();
    let prompt = asked.lock().unwrap()[0].clone();
    assert!(prompt.contains("Snooze an alert") && prompt.contains("Keep it to one tap"));
    assert!(prompt.contains("`header` (header, on desktop and phone)"));
    assert_eq!(ops_named(&seen, "board_ticket").len(), 1);
    let (url, token) = board.opened.lock().unwrap()[0].clone();
    assert!(
        url.ends_with("/v1/repos/7/board?ticket=t1&board=idea-draft-abc") && url.starts_with("ws://"),
        "{url}"
    );
    assert_eq!(token, "tok");
    let sent = board.sent.lock().unwrap().clone();
    assert!(
        sent.len() > 3
            && sent
                .iter()
                .all(|m| m["op"]["kind"] == "add" && m["op"]["coll"] == "things"),
        "only adds"
    );
    let seqs: Vec<u64> = sent.iter().map(|m| m["seq"].as_u64().unwrap()).collect();
    assert_eq!(
        seqs,
        (1..=sent.len() as u64).collect::<Vec<_>>(),
        "each in order, for the hub to ack"
    );
    assert!(sent.iter().all(|m| m["op"]["item"]["id"] != "note-1"));
    assert!(
        sent.iter().all(|m| m["op"]["item"]["chat"].is_string()),
        "the chat's marks"
    );
}

#[tokio::test]
async fn a_layout_that_fails_its_check_is_refused_with_the_reason_and_nothing_is_written() {
    let (client, _) = store(json!({})).await;
    let board = Board::new(vec![]);
    let (ask, _) = session(Ok(json!({"variations": []})));
    let d = Drawing {
        client: &client,
        repo: 7,
        token: "tok",
        transport: &board,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    let why = draw_design_board(&d, "draft-abc", &idea(None)).await.unwrap_err();
    assert!(why.message.starts_with("The layout didn't pass its check: "), "{why}");
    assert!(board.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_session_that_cant_answer_and_a_signed_out_claude_are_reasons_and_nothing_is_written() {
    let (client, _) = store(json!({})).await;
    let board = Board::new(vec![]);
    let (ask, asked) = session(Err(DesignError::conflict("Claude hit its limit.")));
    let d = Drawing {
        client: &client,
        repo: 7,
        token: "tok",
        transport: &board,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    assert_eq!(
        draw_design_board(&d, "draft-abc", &idea(None))
            .await
            .unwrap_err()
            .message,
        "Claude hit its limit."
    );
    let d = Drawing {
        claude_signed_out: true,
        ..d
    };
    let why = draw_design_board(&d, "draft-abc", &idea(None)).await.unwrap_err();
    assert!(why.message.starts_with("Claude is signed out on "), "{why}");
    assert_eq!(asked.lock().unwrap().len(), 1, "a signed-out Claude isn't asked");
    assert!(board.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_store_that_cant_give_a_board_ticket_fails_closed_with_nothing_opened() {
    // A store nothing listens at.
    let gone = Client::new("http://127.0.0.1:1");
    let board = Board::new(vec![]);
    let (ask, asked) = session(Ok(layout()));
    let d = Drawing {
        client: &gone,
        repo: 7,
        token: "tok",
        transport: &board,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    let why = draw_design_board(&d, "draft-abc", &idea(None)).await.unwrap_err();
    assert!(why.message.contains("couldn't reach the backlog store"), "{why}");
    assert!(board.opened.lock().unwrap().is_empty() && asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_redraw_takes_away_the_chats_own_untouched_pieces_and_keeps_the_rest() {
    let (client, _) = store(json!({})).await;
    let first = Board::new(vec![]);
    let (ask, _) = session(Ok(layout()));
    let d = Drawing {
        client: &client,
        repo: 7,
        token: "tok",
        transport: &first,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    draw_design_board(&d, "draft-abc", &idea(None)).await.unwrap();
    let drawn: Vec<Value> = first
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|m| m["op"]["item"].clone())
        .collect();
    let ids: Vec<Value> = drawn.iter().map(|t| t["id"].clone()).collect();
    // A person moves one of them before the redraw.
    let mut things = drawn.clone();
    things[2]["x"] = json!(things[2]["x"].as_f64().unwrap() + 5.0);
    let moved = things[2]["id"].clone();
    things.push(sticky());
    let again = Board::new(things);
    let mut replaces = ids.clone();
    replaces.push(json!("note-1"));
    let (ask, _) = session(Ok(
        json!({"page": "board", "region": "header", "variations": layout()["variations"], "replaces": replaces}),
    ));
    let d = Drawing {
        client: &client,
        repo: 7,
        token: "tok",
        transport: &again,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    draw_design_board(&d, "draft-abc", &idea(Some("Make it a menu")))
        .await
        .unwrap();
    let sent = again.sent.lock().unwrap().clone();
    let removed: Vec<&Value> = sent
        .iter()
        .filter(|m| m["op"]["kind"] == "remove")
        .map(|m| &m["op"]["id"])
        .collect();
    assert_eq!(
        removed.len(),
        ids.len() - 1,
        "its own earlier pieces are taken away, but not the moved one"
    );
    assert!(!removed.contains(&&moved) && !removed.contains(&&json!("note-1")));
    assert!(
        sent.iter().filter(|m| m["op"]["kind"] == "add").count() > 3,
        "the new design goes beside what stays"
    );
}

#[tokio::test]
async fn a_change_asked_for_goes_to_the_session_with_the_idea() {
    let (client, _) = store(json!({})).await;
    let board = Board::new(vec![]);
    let (ask, asked) = session(Ok(layout()));
    let d = Drawing {
        client: &client,
        repo: 7,
        token: "tok",
        transport: &board,
        layouts: &ask,
        timing: fast(),
        claude_signed_out: false,
    };
    draw_design_board(&d, "draft-abc", &idea(Some("Make it a menu")))
        .await
        .unwrap();
    draw_design_board(&d, "draft-abc", &idea(None)).await.unwrap();
    let asked = asked.lock().unwrap().clone();
    assert!(
        asked[0].contains("asked for the designs to change: Make it a menu")
            && !asked[1].contains("asked for the designs to change")
    );
}

fn waiting(extra: Value) -> Value {
    let mut d = json!({"id": "draft-abc", "status": "needs_person", "detail": "awaiting_approval",
        "request": "Let people snooze", "titles": ["Snooze an alert"], "summary": "A snooze button"});
    d.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    d
}

fn reports(seen: &Seen) -> Vec<Value> {
    ops_named(seen, "draft_report")
        .into_iter()
        .map(|b| b["args"].clone())
        .collect()
}

async fn press<'a>(
    client: &'a Client,
    board: &'a Board,
    ask: super::draw::Layouts<'a>,
    a: &'a ClaimNextAgent,
) -> Acting<'a> {
    Acting {
        drawing: Drawing {
            client,
            repo: 7,
            token: "tok",
            transport: board,
            layouts: ask,
            timing: fast(),
            claude_signed_out: false,
        },
        agent: a,
    }
}

#[tokio::test]
async fn skip_the_design_clears_the_flag_and_lets_go_of_the_draft() {
    let (client, seen) = store(waiting(json!({"needsDesign": true, "designNote": "old"}))).await;
    let (board, agent) = (Board::new(vec![]), agent());
    let (ask, _) = session(Ok(layout()));
    skip_design(&press(&client, &board, &ask, &agent).await, "draft-abc")
        .await
        .unwrap();
    let r = reports(&seen);
    assert_eq!(
        (r.len(), &r[0]["needsDesign"], &r[0]["designNote"], &r[0]["token"]),
        (1, &json!(false), &Value::Null, &json!("lease-1"))
    );
    assert!(r[0].as_object().unwrap().contains_key("designNote"), "a null clears it");
    assert_eq!(ops_named(&seen, "draft_release")[0]["args"]["token"], "lease-1");
}

#[tokio::test]
async fn skipping_where_there_is_no_review_changes_nothing_and_a_draft_not_waiting_is_refused() {
    let (client, seen) = store(waiting(json!({}))).await;
    let (board, agent) = (Board::new(vec![]), agent());
    let (ask, _) = session(Ok(layout()));
    skip_design(&press(&client, &board, &ask, &agent).await, "draft-abc")
        .await
        .unwrap();
    assert!(reports(&seen).is_empty() && ops_named(&seen, "draft_release").len() == 1);
    let (client, seen) = store(json!({"id": "draft-abc", "status": "drafting", "needsDesign": true})).await;
    let why = skip_design(&press(&client, &board, &ask, &agent).await, "draft-abc")
        .await
        .unwrap_err();
    assert_eq!(why.message, "Draft draft-abc isn't waiting for approval.");
    assert!(
        reports(&seen).is_empty() && ops_named(&seen, "draft_release").len() == 1,
        "let go of, not changed"
    );
}

#[tokio::test]
async fn try_again_draws_and_clears_the_reason_a_failure_keeps_the_reason_and_both_let_go() {
    let (client, seen) = store(waiting(
        json!({"needsDesign": true, "designNote": "Claude is signed out"}),
    ))
    .await;
    let (board, agent) = (Board::new(vec![]), agent());
    let (ask, asked) = session(Ok(layout()));
    draw_design(
        &press(&client, &board, &ask, &agent).await,
        "draft-abc",
        Some("Make it a menu"),
    )
    .await
    .unwrap();
    assert!(
        asked.lock().unwrap()[0].contains("Snooze an alert\n\nA snooze button"),
        "the plan's title and summary"
    );
    assert!(asked.lock().unwrap()[0].contains("Make it a menu"));
    let r = reports(&seen);
    assert_eq!(
        (r.len(), r[0]["designNote"].clone()),
        (1, Value::Null),
        "cleared once a drawing lands"
    );
    assert_eq!(ops_named(&seen, "draft_release").len(), 1);
    // A drawing that can't be done keeps why, for the card.
    let (client, seen) = store(waiting(json!({"needsDesign": true}))).await;
    let (ask, _) = session(Err(DesignError::conflict("Claude hit its limit.")));
    let why = draw_design(&press(&client, &board, &ask, &agent).await, "draft-abc", None)
        .await
        .unwrap_err();
    assert_eq!(why.message, "Claude hit its limit.");
    assert_eq!(reports(&seen)[0]["designNote"], "Claude hit its limit.");
    assert_eq!(ops_named(&seen, "draft_release").len(), 1);
}

#[tokio::test]
async fn try_again_on_a_plan_with_no_review_is_refused_before_anything_is_drawn() {
    let (client, seen) = store(waiting(json!({}))).await;
    let (board, agent) = (Board::new(vec![]), agent());
    let (ask, asked) = session(Ok(layout()));
    let why = draw_design(&press(&client, &board, &ask, &agent).await, "draft-abc", None)
        .await
        .unwrap_err();
    assert_eq!(
        (why.status, why.message.as_str()),
        (409, "This plan has no design to review.")
    );
    assert!(asked.lock().unwrap().is_empty() && board.opened.lock().unwrap().is_empty());
    assert!(ops_named(&seen, "board_ticket").is_empty() && ops_named(&seen, "draft_release").len() == 1);
}
