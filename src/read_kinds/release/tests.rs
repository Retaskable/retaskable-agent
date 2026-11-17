//! `release_status`: the saved notes as the store answers `releases_get`.
use axum::{Json, Router, routing::post};
use serde_json::json;

use super::super::tests::reading;
use super::*;

/// A store with two saved notes; `gone` is one it doesn't have, `broken` one it fails on.
async fn store() -> String {
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(|Json(body): Json<serde_json::Value>| async move {
            let id = body["args"]["id"].as_str().map(str::to_string);
            match id.as_deref() {
                None => (
                    axum::http::StatusCode::OK,
                    Json(json!({ "value": [
                        { "id": "2026-09-30-notes.md", "title": "September", "from": "2026-09-01", "to": "2026-09-30" },
                        { "id": "2026-08-31-notes.md", "title": "August", "from": "2026-08-01", "to": "2026-08-31" },
                    ] })),
                ),
                Some("gone") => (
                    axum::http::StatusCode::NOT_FOUND,
                    Json(json!({ "error": "not_found", "message": "no note gone" })),
                ),
                Some("broken") => (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "boom", "message": "the store fell over" })),
                ),
                Some(id) => (
                    axum::http::StatusCode::OK,
                    Json(json!({ "value": { "id": id, "title": "T", "from": "a", "to": "b", "body": "# Notes\n" } })),
                ),
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test]
async fn saved_lists_the_notes_as_the_releases_tab_reads_them() {
    let cx = reading("", &store().await);
    assert_eq!(
        release_status(&cx, &json!({ "part": "saved" })).await.unwrap(),
        json!([
            { "path": "2026-09-30-notes.md", "from": "2026-09-01", "to": "2026-09-30", "stored": true },
            { "path": "2026-08-31-notes.md", "from": "2026-08-01", "to": "2026-08-31", "stored": true },
        ])
    );
}

#[tokio::test]
async fn last_is_the_newest_saved_note() {
    let cx = reading("", &store().await);
    assert_eq!(
        release_status(&cx, &json!({ "part": "last" })).await.unwrap(),
        json!({ "last": { "at": "2026-09-30", "label": "2026-09-30-notes" } })
    );
}

#[tokio::test]
async fn a_note_answers_its_markdown_and_a_missing_or_broken_one_says_why() {
    let cx = reading("", &store().await);
    assert_eq!(
        release_status(&cx, &json!({ "part": "note", "id": "2026-09-30-notes.md" })).await,
        Ok(json!({ "markdown": "# Notes\n" }))
    );
    let gone = release_status(&cx, &json!({ "part": "note", "id": "gone" }))
        .await
        .unwrap_err();
    assert!(
        matches!(&gone, ReadError::Refused(w) if w.starts_with("Couldn't read gone: ")),
        "{gone:?}"
    );
    let broken = release_status(&cx, &json!({ "part": "note", "id": "broken" }))
        .await
        .unwrap_err();
    assert!(
        matches!(&broken, ReadError::Failed(w) if w.starts_with("Couldn't read broken: ")),
        "{broken:?}"
    );
    assert_eq!(
        release_status(&cx, &json!({ "part": "note" })).await,
        Err(ReadError::Refused("That note isn't in a shared backlog.".into()))
    );
}

#[tokio::test]
async fn a_store_that_cannot_be_reached_is_never_answered_from_a_guess() {
    let cx = reading("", "http://127.0.0.1:1");
    for part in ["saved", "last"] {
        assert_eq!(
            release_status(&cx, &json!({ "part": part })).await,
            Err(ReadError::Refused(NOT_REACHED.into())),
            "{part}"
        );
    }
    assert_eq!(
        release_status(&cx, &json!({ "part": "other" })).await,
        Err(ReadError::Refused(
            "release_status takes a part: last, saved or note.".into()
        ))
    );
}
