//! The budget as a `Client` meets it: a store that answers 429 holds every project's calls, not just the one refused.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::json;

use super::*;
use crate::backlog::Client;

/// A store that answers 429 `rate_limited` with `Retry-After: after` until `limited` is cleared, counting its calls.
async fn limiting(after: &'static str) -> (String, Arc<AtomicU32>, Arc<std::sync::atomic::AtomicBool>) {
    let calls = Arc::new(AtomicU32::new(0));
    let limited = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (c, l) = (calls.clone(), limited.clone());
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move || {
            let (c, l) = (c.clone(), l.clone());
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                if l.load(Ordering::SeqCst) {
                    let mut res = (
                        StatusCode::TOO_MANY_REQUESTS,
                        axum::Json(json!({ "error": "rate_limited", "message": "Too many requests to the backlog: wait a minute and try again." })),
                    )
                        .into_response();
                    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static(after));
                    res
                } else {
                    axum::Json(json!({ "value": [] })).into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), calls, limited)
}

fn budgeted(base: String) -> Client {
    let mut client = Client::new(base);
    client.budget = Arc::new(Budget::limited_store());
    client
}

#[tokio::test]
async fn one_projects_429_holds_every_projects_calls_without_sending_them() {
    let (base, calls, _) = limiting("200").await;
    let client = budgeted(base);
    let first = client.list(1, Some("t")).await.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The store's own 429 carries a code; the wait is still read from it.
    assert_eq!(first.code(), Some("rate_limited"));
    assert_eq!(first.retry_after(), Some(Duration::from_secs(200)));
    // Other projects and other ops: refused here, with the wait left, and not sent.
    for repo in 2..=6 {
        let e = client.drafts(repo, Some("t")).await.unwrap_err();
        assert!(e.retry_after().is_some_and(|w| w > Duration::from_secs(190)), "{e}");
    }
    assert!(client.get_bytes(7, Some("t"), "pictures/p").await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "nothing was sent while held");
}

#[tokio::test(start_paused = true)]
async fn a_short_hold_is_waited_out_and_the_calls_then_go() {
    let (base, calls, limited) = limiting("60").await;
    let client = budgeted(base);
    assert!(client.list(1, Some("t")).await.is_err());
    limited.store(false, Ordering::SeqCst);
    let start = tokio::time::Instant::now();
    client.drafts(2, Some("t")).await.unwrap();
    assert!(start.elapsed() >= Duration::from_secs(59), "{:?}", start.elapsed());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
