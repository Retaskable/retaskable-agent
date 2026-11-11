use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

use super::*;

// SavedReleases

#[derive(Default)]
struct Fake {
    ops: Vec<Value>,
}

async fn handle(State(fake): State<Arc<Mutex<Fake>>>, Json(req): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut fake = fake.lock().unwrap();
    fake.ops.push(req.clone());
    let row = json!({ "id": "2026-09-30", "title": "Notes", "from": "2026-09-01", "to": "2026-09-30",
                      "createdAt": 1, "by": "jo" });
    match (req["op"].as_str(), req["args"]["id"].as_str()) {
        (Some("releases_get"), Some("2026-09-30")) => {
            let mut one = row.clone();
            one["body"] = json!("# Notes");
            (StatusCode::OK, Json(json!({ "value": one })))
        }
        (Some("releases_get"), Some(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_found", "message": "no such note" })),
        ),
        (Some("releases_get"), None) => (StatusCode::OK, Json(json!({ "value": [row] }))),
        _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad_request" }))),
    }
}

async fn fake_store(fake: Arc<Mutex<Fake>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route("/v1/repos/{id}/ops", post(handle)).with_state(fake);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn saved(client: &Client) -> SavedReleases<'_> {
    SavedReleases {
        client,
        repo_id: 1,
        token: Some("tok"),
    }
}

#[tokio::test]
async fn list_reads_the_store_and_offers_it_nothing() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let client = Client::new(fake_store(fake.clone()).await);
    let rows = saved(&client).list().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "2026-09-30");
    assert!(rows[0].body.is_none());
    let sent = fake
        .lock()
        .unwrap()
        .ops
        .iter()
        .map(|o| o["op"].clone())
        .collect::<Vec<_>>();
    assert_eq!(sent, [json!("releases_get")], "no releases_offer");
}

#[tokio::test]
async fn a_store_that_cant_be_reached_gives_none() {
    let client = Client::new("http://127.0.0.1:1".to_string());
    assert!(saved(&client).list().await.is_none());
}

#[tokio::test]
async fn get_reads_a_saved_note_with_its_markdown_and_a_missing_one_is_not_found() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let client = Client::new(fake_store(fake.clone()).await);
    let releases = saved(&client);
    assert_eq!(
        releases.get("2026-09-30").await.unwrap().body.as_deref(),
        Some("# Notes")
    );
    assert_eq!(releases.get("nope").await.unwrap_err().code(), Some("not_found"));
}
