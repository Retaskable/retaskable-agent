//! The pass reads the store's pause (`facts_get`) into `links.store_paused`.
use super::*;
use crate::view::StorePause;
use axum::response::IntoResponse;

/// A store that lists nothing and answers `facts_get` with `facts` (a 500 for `None`), noting each op.
async fn store(facts: Option<Value>) -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, facts) = (seen.clone(), facts.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push(op.clone());
                match (op.as_str(), facts) {
                    ("facts_get", None) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    ("facts_get", Some(v)) => Json(json!({ "value": v })).into_response(),
                    _ => Json(json!({ "value": [] })).into_response(),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

async fn pass(facts: Option<Value>) -> (StorePause, Vec<String>) {
    let (base, asked) = store(facts).await;
    let mut rig = rig(2, &[&base]);
    rig.machine.pass().await;
    let got = rig.machine.links.store_paused.get("p0");
    let asked = asked.lock().unwrap().clone();
    (got, asked)
}

#[tokio::test]
async fn the_store_pause_is_recorded_with_the_stores_words_or_the_default() {
    let (got, asked) = pass(Some(
        json!({ "paused": true, "pausedReason": "Paused by @james for everyone" }),
    ))
    .await;
    assert_eq!(got, StorePause::Paused("Paused by @james for everyone".into()));
    assert!(asked.contains(&"facts_get".to_string()));
    assert_eq!(
        pass(Some(json!({ "paused": true }))).await.0,
        StorePause::Paused("Paused for everyone".into())
    );
    assert_eq!(pass(Some(json!({ "paused": false }))).await.0, StorePause::NotPaused);
}

#[tokio::test]
async fn a_failed_facts_read_is_unknown() {
    assert_eq!(pass(None).await.0, StorePause::Unknown);
}

#[tokio::test]
async fn a_changed_store_pause_wakes_the_watch_and_a_same_one_doesnt() {
    let state = crate::view::StorePauseState::default();
    let mut watch = state.subscribe();
    watch.mark_unchanged();
    state.set("p", StorePause::Paused("x".into()));
    assert!(watch.has_changed().unwrap());
    watch.mark_unchanged();
    state.set("p", StorePause::Paused("x".into()));
    assert!(!watch.has_changed().unwrap());
    state.set("p", StorePause::NotPaused);
    assert!(watch.has_changed().unwrap());
}
