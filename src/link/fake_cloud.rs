//! A fake cloud for tests that link through the settings page: both device endpoints (`/api/device` and `/v1/device`),
//! a code that waits until the test approves, denies or expires it, and every body it was sent. Real HTTP on loopback,
//! so no test waits on a fixed sleep: a test waits on what it is watching changing.
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

/// The device code and refresh token it hands out: a test checks they are never in what the page is sent.
pub const DEVICE_CODE: &str = "DEVICE-SECRET-1";
pub const REFRESH: &str = "REFRESH-SECRET-1";
pub const USER_CODE: &str = "ABCD-1234";

#[derive(Default)]
pub struct FakeCloud {
    base: Mutex<String>,
    /// What `/token` answers for the device code; `None` is "authorization_pending".
    answer: Mutex<Option<(StatusCode, Value)>>,
    /// Every body POSTed to `/code`.
    pub codes: Mutex<Vec<Value>>,
}

impl FakeCloud {
    /// The person approves the code.
    pub fn approve(&self) {
        let ok = json!({ "refresh_token": REFRESH, "agent_id": 7 });
        *self.answer.lock().unwrap() = Some((StatusCode::OK, ok));
    }

    /// The store answers the poll with `error` (`access_denied`, `expired_token`).
    pub fn refuse(&self, error: &str) {
        *self.answer.lock().unwrap() = Some((StatusCode::BAD_REQUEST, json!({ "error": error })));
    }

    /// How many codes were asked for.
    pub fn asked(&self) -> usize {
        self.codes.lock().unwrap().len()
    }

    pub fn base(&self) -> String {
        self.base.lock().unwrap().clone()
    }
}

async fn code(State(f): State<Arc<FakeCloud>>, Json(body): Json<Value>) -> Json<Value> {
    f.codes.lock().unwrap().push(body);
    let base = f.base();
    Json(json!({
        "device_code": DEVICE_CODE,
        "user_code": USER_CODE,
        "verification_uri": format!("{base}/link"),
        "verification_uri_complete": format!("{base}/link?code={USER_CODE}"),
        "interval": 1,
        "expires_in": 600,
    }))
}

async fn token(State(f): State<Arc<FakeCloud>>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if body["grant_type"] == "refresh_token" {
        let ok = body["refresh_token"] == REFRESH;
        return match ok {
            true => (
                StatusCode::OK,
                Json(json!({ "access_token": "ACCESS-1", "refresh_token": REFRESH, "expires_in": 3600 })),
            ),
            false => (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" }))),
        };
    }
    let (status, json) = f
        .answer
        .lock()
        .unwrap()
        .clone()
        .unwrap_or((StatusCode::BAD_REQUEST, json!({ "error": "authorization_pending" })));
    (status, Json(json))
}

/// Starts one on a free loopback port.
pub async fn spawn() -> Arc<FakeCloud> {
    let fake = Arc::new(FakeCloud::default());
    let app = Router::new()
        .route("/api/device/code", post(code))
        .route("/api/device/token", post(token))
        .route("/v1/device/code", post(code))
        .route("/v1/device/token", post(token))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    *fake.base.lock().unwrap() = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    fake
}
