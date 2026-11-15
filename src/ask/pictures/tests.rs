use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use serde_json::json;

use super::*;

const A: &str = "0a1b2c3d-0000-4000-8000-00000000000a.png";
const B: &str = "0a1b2c3d-0000-4000-8000-00000000000b.webp";

type Asked = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// A store that serves `A` and `B` as their own names and refuses anything else.
async fn pictures() -> (Client, Asked) {
    let asked: Asked = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{repo}/pictures/{id}",
            get(
                |State(asked): State<Asked>, UrlPath((_, id)): UrlPath<(u64, String)>, headers: HeaderMap| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(String::from);
                    asked.lock().unwrap().push((id.clone(), auth));
                    if id == A || id == B {
                        (StatusCode::OK, id.into_bytes())
                    } else {
                        (StatusCode::NOT_FOUND, b"{\"error\":\"not_found\"}".to_vec())
                    }
                },
            ),
        )
        .with_state(asked.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Client::new(base), asked)
}

#[test]
fn only_checked_ids_are_taken_from_the_row() {
    let args = json!({ "pictureIds": [A, "../../etc/passwd", 7, "short.png", B, format!("{}.exe", &A[..36]),
        format!("{}.PNG", &A[..36])] });
    assert_eq!(ids_of(&args), [A, B]);
    assert!(ids_of(&json!({})).is_empty());
    assert!(ids_of(&json!({ "pictureIds": "x" })).is_empty());
}

#[tokio::test]
async fn pictures_are_saved_listed_and_removed_when_dropped() {
    let (client, asked) = pictures().await;
    let dir = tempfile::tempdir().unwrap();
    let saved = fetch(&client, 1, Some("tok"), &[A.into(), B.into()], dir.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read(dir.path().join(A)).unwrap(), A.as_bytes());
    let block = saved.block();
    assert!(block.starts_with("Attached images (look at each with your Read tool before acting on this):\n- "));
    assert!(block.contains(A) && block.contains(B));
    assert_eq!(
        asked.lock().unwrap()[0],
        (A.to_string(), Some("Bearer tok".to_string()))
    );
    drop(saved);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn no_pictures_is_an_empty_block_and_touches_nothing() {
    let (client, asked) = pictures().await;
    let dir = tempfile::tempdir().unwrap();
    let saved = fetch(&client, 1, None, &[], &dir.path().join("never")).await.unwrap();
    assert_eq!(saved.block(), "");
    assert!(asked.lock().unwrap().is_empty());
    assert!(!dir.path().join("never").exists());
}

#[tokio::test]
async fn a_picture_the_store_refuses_fails_the_lot_and_removes_what_was_saved() {
    let (client, _) = pictures().await;
    let dir = tempfile::tempdir().unwrap();
    let missing = "0a1b2c3d-0000-4000-8000-0000000000ff.png".to_string();
    let why = fetch(&client, 1, None, &[A.into(), missing], dir.path())
        .await
        .unwrap_err();
    assert_eq!(why, "Couldn't read the attached pictures.");
    // `saved` was dropped on the way out.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
