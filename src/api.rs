//! The agent's port (Retaskable.com docs/specs/agent.md section 8): `GET /api/health`, so a script or a person can
//! ask whether the agent is running. retaskable.com is the screen for everything else, and never a command here:
//! every other path is a 404.
use axum::{
    Json, Router,
    http::{Method, StatusCode, Uri},
    routing::get,
};
use serde_json::{Value, json};

pub fn router(version: &'static str) -> Router {
    Router::new()
        .route(
            "/api/health",
            // Any other method is a 404 too, not axum's empty 405.
            get(move || async move { Json(json!({ "ok": true, "version": version })) }).fallback(not_found),
        )
        .fallback(not_found)
}

async fn not_found(_method: Method, _uri: Uri) -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn call(method: &str, path: &str) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let res = router("1.2.3").oneshot(req).await.unwrap();
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
