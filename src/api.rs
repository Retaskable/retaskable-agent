//! The agent's port (Retaskable.com docs/specs/agent.md section 8): `GET /api/health` and the
//! settings page's own routes (`settings_server`), so a script or a person can ask whether the
//! agent is running and a person can change its settings. retaskable.com is the screen for
//! everything else, and never a command here: every other path is a 404.
use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::{
    Json, Router,
    http::{Method, StatusCode, Uri},
    routing::get,
};
use serde_json::json;

use crate::settings_server::{self, SettingsServer};

pub fn router(version: &'static str, settings: Arc<SettingsServer>) -> Router {
    Router::new()
        .route(
            "/api/health",
            // Any other method is a 404 too, not axum's empty 405.
            get(move || async move { Json(json!({ "ok": true, "version": version })) }).fallback(not_found),
        )
        .merge(settings_server::router(settings))
        .fallback(not_found)
}

/// Shared with `settings_server`, so a request its Host/Origin checks refuse gives away nothing
/// about which paths exist: the same 404 as a path this agent doesn't serve at all.
pub(crate) fn not_found_response() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response()
}

async fn not_found(_method: Method, _uri: Uri) -> Response {
    not_found_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    async fn call(method: &str, path: &str) -> (StatusCode, Value) {
        let (settings, _dir) = settings_server::test_server();
        let req = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let res = router("1.2.3", settings).oneshot(req).await.unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn health_answers_ok_and_the_version() {
        assert_eq!(
            call("GET", "/api/health").await,
            (StatusCode::OK, json!({ "ok": true, "version": "1.2.3" }))
        );
    }

    #[tokio::test]
    async fn the_settings_page_is_merged_in_alongside_health() {
        let (settings, _dir) = settings_server::test_server();
        let req = Request::builder()
            .method("GET")
            .uri("/settings")
            .header("Host", "127.0.0.1:4700")
            .body(Body::empty())
            .unwrap();
        let res = router("1.2.3", settings).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn every_other_route_is_a_404() {
        for (method, path) in [
            ("GET", "/api/state"),
            ("POST", "/api/health"),
            ("PUT", "/api/health"),
            ("DELETE", "/api/health"),
            ("OPTIONS", "/api/health"),
            ("POST", "/api/items/4/skip"),
            ("GET", "/"),
            ("GET", "/board"),
        ] {
            assert_eq!(
                call(method, path).await,
                (StatusCode::NOT_FOUND, json!({ "error": "Not found" })),
                "{method} {path}"
            );
        }
    }
}
