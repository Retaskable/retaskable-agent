use super::*;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<Value>>>;

/// A fake store answering every op with `status` and `reply`, recording each request body.
async fn store(status: u16, reply: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                move |State((seen, reply)): State<(Seen, Value)>, Json(body): Json<Value>| async move {
                    seen.lock().unwrap().push(body);
                    (StatusCode::from_u16(status).unwrap(), Json(reply))
                },
            ),
        )
        .with_state((seen.clone(), reply));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn agent(tags: &[&str]) -> RunTakeAgent {
    RunTakeAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

#[tokio::test]
async fn run_take_sends_kinds_and_reads_a_run() {
    let (base, seen) = store(
        200,
        json!({ "value": { "token": "t1", "kind": "build", "extra": 1,
            "piece": { "title": "T", "what": "W", "doneWhen": "D", "attempts": [{"k":1}],
                       "branch": "b", "change": {"n": 2}, "id": "p1" } } }),
    )
    .await;
    let c = Client::new(base);
    let a = c
        .run_take(
            1,
            Some("tok"),
            &agent(&[]),
            RunWant::Kinds(&[RunKind::Build, RunKind::Watch]),
        )
        .await
        .unwrap();
    let RunTakeAnswer::Run(run) = a else { panic!("{a:?}") };
    assert_eq!(run.token, "t1");
    assert_eq!(run.kind, RunKind::Build);
    assert_eq!(run.piece.done_when, "D");
    assert_eq!(run.piece.branch.as_deref(), Some("b"));
    assert_eq!(run.piece.attempts.len(), 1);
    assert_eq!(run.piece.other["id"], "p1");
    assert_eq!(run.other["extra"], 1);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "run_take");
    assert_eq!(sent["args"]["kinds"], json!(["build", "watch"]));
    assert!(sent["args"]["agent"].get("tags").is_none());
    assert_eq!(sent["args"]["agent"]["baseBranch"], "main");
}

#[tokio::test]
async fn run_take_by_piece_sends_tags_when_there_are_some() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let a = Client::new(base)
        .run_take(1, None, &agent(&["gpu"]), RunWant::Piece("p1"))
        .await
        .unwrap();
    assert_eq!(a, RunTakeAnswer::Nothing);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["args"]["piece"], "p1");
    assert!(sent["args"].get("kinds").is_none());
    assert_eq!(sent["args"]["agent"]["tags"], json!(["gpu"]));
}

#[tokio::test]
async fn run_take_reads_waiting_for() {
    let (base, _) = store(200, json!({ "value": { "waitingFor": ["gpu"] } })).await;
    let a = Client::new(base)
        .run_take(1, None, &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap();
    assert_eq!(a, RunTakeAnswer::WaitingFor(json!(["gpu"])));
}

#[tokio::test]
async fn run_take_with_an_unreadable_run_is_an_error() {
    let (base, _) = store(200, json!({ "value": { "token": "t" } })).await;
    let e = Client::new(base)
        .run_take(1, None, &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap_err();
    assert!(matches!(e, BacklogError::BadAnswer { .. }));
}

#[tokio::test]
async fn run_beat_keeps_stop_apart_from_ok() {
    let (base, seen) = store(200, json!({ "value": { "a": "ok", "b": "stop" } })).await;
    let v = Client::new(base)
        .run_beat(1, None, &["a".to_string(), "b".to_string()])
        .await
        .unwrap();
    assert_eq!(v["a"], BeatVerdict::Ok);
    assert_eq!(v["b"], BeatVerdict::Stop);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "run_beat");
    assert_eq!(sent["args"]["tokens"], json!(["a", "b"]));
}

#[tokio::test]
async fn run_done_sends_outcome_and_result() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let c = Client::new(base);
    c.run_done(1, None, "t", &RunOutcome::PrOpened, json!({ "branch": "b" }))
        .await
        .unwrap();
    c.run_done(1, None, "t", &RunOutcome::Failed("boom".into()), Value::Null)
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "run_done");
    assert_eq!(
        seen[0]["args"],
        json!({ "token": "t", "outcome": "pr_opened", "result": { "branch": "b" } })
    );
    assert_eq!(seen[1]["args"]["outcome"], "failed");
    assert_eq!(seen[1]["args"]["result"], json!({ "why": "boom" }));
}

#[tokio::test]
async fn run_done_tells_stale_run_from_other_refusals() {
    let (base, _) = store(409, json!({ "error": "stale_run", "message": "gone" })).await;
    let e = Client::new(base)
        .run_done(1, None, "t", &RunOutcome::Green, Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(e, RunDoneError::Stale));
    let (base, _) = store(400, json!({ "error": "bad_request" })).await;
    let e = Client::new(base)
        .run_done(1, None, "t", &RunOutcome::Green, Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(e, RunDoneError::Other(BacklogError::BadAnswer { .. })));
}

#[tokio::test]
async fn runs_drop_sends_the_op() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    Client::new(base).runs_drop(1, None).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0]["op"], "runs_drop");
}

#[tokio::test]
async fn an_unreachable_store_is_unreachable() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let c = Client::new(format!("http://{addr}"));
    assert!(matches!(c.runs_drop(1, None).await, Err(BacklogError::Unreachable(_))));
    assert!(matches!(
        c.run_beat(1, None, &[]).await,
        Err(BacklogError::Unreachable(_))
    ));
    assert!(matches!(
        c.run_take(1, None, &agent(&[]), RunWant::Kinds(&[])).await,
        Err(BacklogError::Unreachable(_))
    ));
    assert!(matches!(
        c.run_done(1, None, "t", &RunOutcome::Green, Value::Null).await,
        Err(RunDoneError::Other(BacklogError::Unreachable(_)))
    ));
}
