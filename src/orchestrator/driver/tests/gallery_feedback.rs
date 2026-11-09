//! The `gallery_feedback` step (`driver/gallery_feedback.rs`): the queued feedback becomes one `draft_queue`, then an
//! `ack`; a store that refuses gives a `nack`.
use super::*;
use crate::view::GalleryFeedbackCommand;

async fn store(refuse: bool) -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                if refuse && op == "draft_queue" {
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({ "error": "down", "message": "down" })),
                    );
                }
                let value = if op == "draft_queue" {
                    json!({ "id": "d1" })
                } else {
                    json!([])
                };
                (axum::http::StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn feedback(do_next: bool) -> GalleryFeedbackCommand {
    GalleryFeedbackCommand {
        project: "p0".into(),
        command_id: "c1".into(),
        request: "Fix the hulls".into(),
        do_next,
        by: "jc".into(),
    }
}

async fn run_one(refuse: bool, do_next: bool) -> (Value, Vec<(String, Value)>, bool) {
    let (base, asked) = store(refuse).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    let queue = rig.machine.links.item_commands.clone();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["gallery-feedback-command"]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    queue.set_out("p0", Some(tx));
    queue.push_feedback(feedback(do_next));
    rig.machine.pass().await;
    let on = queue.flag_on("p0", "gallery-feedback-command");
    let answer = rx.try_recv().unwrap();
    let asked = asked.lock().unwrap().clone();
    (answer, asked, on)
}

#[tokio::test]
async fn queued_feedback_becomes_one_draft_and_an_ack() {
    let (answer, asked, on) = run_one(false, true).await;
    assert!(on);
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    let drafts: Vec<&Value> = asked
        .iter()
        .filter(|(op, _)| op == "draft_queue")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(drafts.len(), 1);
    let d = drafts[0];
    assert_eq!(d["request"], "Fix the hulls");
    assert_eq!(d["requestedBy"], "jc");
    assert_eq!(d["intake"], json!({ "doNext": true }));
}

#[tokio::test]
async fn feedback_without_do_next_has_no_intake() {
    let (_, asked, _) = run_one(false, false).await;
    let (_, d) = asked.iter().find(|(op, _)| op == "draft_queue").unwrap();
    assert!(d.get("intake").is_none_or(Value::is_null));
}

#[tokio::test]
async fn a_store_error_is_a_nack_with_the_reason() {
    let (answer, ..) = run_one(true, true).await;
    assert_eq!(answer["type"], "nack");
    assert!(answer["reason"].as_str().unwrap().contains("Couldn't queue the draft"));
}
