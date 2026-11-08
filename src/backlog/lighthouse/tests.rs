use super::*;
use crate::backlog::tests::store;
use axum::{Json, Router, extract::Path, http::StatusCode, routing::get};

#[tokio::test]
async fn lighthouse_set_raw_sends_the_fact_and_reads_the_summary() {
    let (base, seen) = store(
        200,
        json!({ "value": { "kind": "lighthouse", "data": { "day": "2026-11-08", "view": { "scores": {} } } } }),
    )
    .await;
    let raw = json!({ "day": "2026-11-08", "firstPage": "/", "pages": [], "newFindings": 2, "fixQueued": ["csp"] });
    let got = Client::new(base)
        .lighthouse_set_raw(7, Some("tok"), &raw, "box-1")
        .await
        .unwrap();
    assert_eq!(got.day, "2026-11-08");
    assert_eq!(got.view, json!({ "scores": {} }));
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "fact_set");
    assert_eq!(seen[0]["args"]["kind"], "lighthouse_raw");
    assert_eq!(seen[0]["args"]["machine"], "box-1");
    assert_eq!(seen[0]["args"]["data"], raw);
}

#[tokio::test]
async fn team_drafts_counts_open_chore_drafts_like_chore_next() {
    let row = |id: &str, status: &str, detail: Value, chore: bool| json!({ "id": id, "status": status, "detail": detail, "chore": chore, "addedAt": 1000, "updatedAt": 2000 });
    let (base, seen) = store(
        200,
        json!({ "value": [
            row("a", "queued", Value::Null, true),
            row("b", "needs_person", Value::Null, true),
            row("c", "drafting", json!("failed"), true),
            row("d", "merged", Value::Null, true),
            row("e", "drafting", Value::Null, false),
        ] }),
    )
    .await;
    let team = Client::new(base).team_drafts(1, None).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0]["op"], "drafts");
    assert_eq!(team.open, 2);
    let d = team.get("d").unwrap();
    assert_eq!((d.status.as_str(), d.added_at, d.updated_at), ("merged", 1000, 2000));
    assert!(team.get("zzz").is_none());
}

#[tokio::test]
async fn a_store_error_is_an_error_not_an_empty_list_or_zero_count() {
    let (base, _) = store(500, json!({ "error": "internal", "message": "down" })).await;
    let c = Client::new(base);
    let err = c.team_drafts(1, None).await.unwrap_err();
    assert_eq!(err.code(), Some("internal"));
    assert!(c.lighthouse_set_raw(1, None, &json!({}), "m").await.is_err());
    // An answer with no value isn't an empty list either.
    let (base, _) = store(200, json!({})).await;
    assert!(Client::new(base).team_drafts(1, None).await.is_err());
}

async fn reports_store() -> String {
    let app = Router::new()
        .route(
            "/v1/repos/{id}/lighthouse",
            get(|| async { Json(json!({ "value": { "days": { "2026-11-07": ["a.json"] } } })) }),
        )
        .route(
            "/v1/repos/{id}/lighthouse/{day}/{file}",
            get(|Path((_, day, file)): Path<(u64, String, String)>| async move { format!("{day}:{file}") }).post(
                |Path((_, _, file)): Path<(u64, String, String)>, body: String| async move {
                    if file == "bad.json" {
                        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad_request" })));
                    }
                    (
                        StatusCode::OK,
                        Json(json!({ "value": { "hash": format!("h{}", body.len()) } })),
                    )
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn reports_are_listed_read_back_and_uploaded_with_their_hash() {
    let c = Client::new(reports_store().await);
    let days = c.saved_runs(1, Some("tok")).await.unwrap();
    assert_eq!(days["2026-11-07"], ["a.json"]);
    let bytes = c.saved_report(1, None, "2026-11-07", "a b.json").await.unwrap();
    assert_eq!(bytes, b"2026-11-07:a b.json");
    let hash = c
        .upload_report(1, None, "2026-11-08", "a.json", b"{\"x\":1}".to_vec())
        .await
        .unwrap();
    assert_eq!(hash, "h7");
    let err = c
        .upload_report(1, None, "2026-11-08", "bad.json", Vec::new())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("bad_request"));
}

#[tokio::test]
async fn an_unreachable_store_fails_the_report_calls() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let c = Client::new(format!("http://{addr}"));
    assert!(matches!(c.saved_runs(1, None).await, Err(BacklogError::Unreachable(_))));
    assert!(c.upload_report(1, None, "d", "f", vec![]).await.is_err());
}
