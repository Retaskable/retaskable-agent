//! What the run loop does after the store answers empty or refuses: waits like the TypeScript agent (`pieceRuns.ts`), and
//! not a `merge_next` and `run_take` on every poll (a live agent sent 10,598 of them in a day and was answered 429).
use super::*;
use axum::response::IntoResponse;

/// A store whose `merge_next` and `run_take` answer null (nothing for this computer), or `429` with `Retry-After`
/// when `limited` is set; every op is counted.
async fn counting_store() -> (String, Shared_) {
    let script: Shared_ = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(|State(s): State<Shared_>, Json(body): Json<Value>| async move {
                let mut s = s.lock().unwrap();
                s.log.push(body.clone());
                let op = body["op"].as_str().unwrap_or_default();
                if s.down && (op == "merge_next" || op == "run_take") {
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert("retry-after", "300".parse().unwrap());
                    let body = Json(json!({ "error": null, "message": "Too many requests to the backlog" }));
                    return (StatusCode::TOO_MANY_REQUESTS, headers, body).into_response();
                }
                let value = if op == "merge_open" {
                    json!({ "turn": null })
                } else {
                    Value::Null
                };
                (StatusCode::OK, Json(json!({ "value": value }))).into_response()
            }),
        )
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), script)
}

async fn quiet_rig() -> (Rig, Shared_, FakeRuns) {
    let (base, script) = counting_store().await;
    let mut rig = rig(1, &[&base]);
    rig.machine.projects[0].mode = OrchestratorMode::Runs;
    (rig, script, FakeRuns::new(&[RunKind::Build, RunKind::Merge]))
}

fn asks(script: &Shared_) -> (usize, usize) {
    let s = script.lock().unwrap();
    (s.args("merge_next").len(), s.args("run_take").len())
}

#[tokio::test]
async fn after_none_the_store_is_not_asked_again_every_poll() {
    let (mut rig, script, starter) = quiet_rig().await;
    let t0 = Instant::now();
    pass(&mut rig, &starter, t0).await;
    assert_eq!(asks(&script), (1, 1));
    // A poll every three seconds for a minute: nothing more is sent.
    for k in 1..20 {
        pass(&mut rig, &starter, t0 + Duration::from_secs(3 * k)).await;
    }
    assert_eq!(asks(&script), (1, 1), "an empty answer holds the asking");
    // The minute's backstop catches a push that was lost.
    pass(&mut rig, &starter, t0 + Duration::from_secs(61)).await;
    assert_eq!(asks(&script), (2, 2));
}

#[tokio::test]
async fn a_work_available_push_ends_the_wait_at_once() {
    let (mut rig, script, starter) = quiet_rig().await;
    let t0 = Instant::now();
    pass(&mut rig, &starter, t0).await;
    pass(&mut rig, &starter, t0 + Duration::from_secs(3)).await;
    assert_eq!(asks(&script), (1, 1));
    rig.wake();
    pass(&mut rig, &starter, t0 + Duration::from_secs(4)).await;
    assert_eq!(asks(&script), (2, 2));
}

#[tokio::test]
async fn a_run_ending_ends_the_wait_too() {
    let (mut rig, script) = setup(2).await;
    let starter = FakeRuns::new(&[RunKind::Build]);
    let t0 = Instant::now();
    script.lock().unwrap().takes = VecDeque::from([run("a", "build")]);
    pass(&mut rig, &starter, t0).await;
    assert_eq!(starter.started(), ["a"]);
    // The second lane's ask was answered empty, which holds the asking.
    script.lock().unwrap().takes = VecDeque::from([run("b", "build")]);
    pass(&mut rig, &starter, t0 + Duration::from_secs(3)).await;
    assert_eq!(starter.started(), ["a"]);
    // A run's end is a lane free: asked again at once.
    starter.finish("a", RunOutcome::PrOpened);
    pass(&mut rig, &starter, t0 + Duration::from_secs(6)).await;
    assert_eq!(starter.started(), ["a", "b"]);
}

#[tokio::test]
async fn a_429_backs_off_for_the_retry_after_and_a_push_does_not_cut_it_short() {
    let (mut rig, script, starter) = quiet_rig().await;
    script.lock().unwrap().down = true; // answers merge_next and run_take with 429, Retry-After 300
    let t0 = Instant::now();
    pass(&mut rig, &starter, t0).await;
    assert_eq!(asks(&script), (1, 0), "the refused merge_next ends the pass's asking");
    rig.wake();
    for k in 1..30 {
        pass(&mut rig, &starter, t0 + Duration::from_secs(10 * k)).await;
    }
    assert_eq!(asks(&script), (1, 0), "nothing for the 300 seconds the store asked");
    script.lock().unwrap().down = false;
    pass(&mut rig, &starter, t0 + Duration::from_secs(301)).await;
    assert_eq!(asks(&script).0, 2);
}
