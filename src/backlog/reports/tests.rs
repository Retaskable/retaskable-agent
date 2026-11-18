use std::sync::{Arc, Mutex};

use axum::{Router, body::Bytes, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use serde_json::json;

use super::*;

type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

async fn store(status: u16, reply: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/lighthouse/{day}/{file}",
            post(
                move |State((seen, reply)): State<(Seen, Value)>,
                      axum::extract::Path((_, day, file)): axum::extract::Path<(u64, String, String)>,
                      body: Bytes| async move {
                    seen.lock().unwrap().push((format!("{day}/{file}"), body.to_vec()));
                    (StatusCode::from_u16(status).unwrap(), axum::Json(reply)).into_response()
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

#[tokio::test]
async fn an_upload_answers_the_hash_the_store_holds() {
    let (base, seen) = store(200, json!({ "value": { "hash": "abc123" } })).await;
    let hash = Client::new(base)
        .upload_report(7, Some("t"), "2026-11-17", "home mobile.json", b"{}".to_vec())
        .await
        .unwrap();
    assert_eq!(hash, "abc123");
    // The file name's space is encoded, and the store decodes it back.
    assert_eq!(
        seen.lock().unwrap()[0],
        ("2026-11-17/home mobile.json".to_string(), b"{}".to_vec())
    );
}

#[tokio::test]
async fn a_refusal_carries_the_stores_words_and_status() {
    let (base, _) = store(413, json!({ "error": "too_big", "message": "That report is too big." })).await;
    let err = Client::new(base)
        .upload_report(7, None, "2026-11-17", "a.json", vec![0; 4])
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("too_big"));
    assert!(err.to_string().contains("too big"), "{err}");
}

#[tokio::test]
async fn an_answer_without_a_hash_is_not_a_success() {
    let (base, _) = store(200, json!({ "value": {} })).await;
    let err = Client::new(base)
        .upload_report(7, None, "2026-11-17", "a.json", vec![])
        .await
        .unwrap_err();
    assert!(matches!(err, BacklogError::BadAnswer { status: 200, .. }));
}
