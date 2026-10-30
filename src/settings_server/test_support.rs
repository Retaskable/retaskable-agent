//! Shared HTTP test harness for [`super::tests`] and [`super::save::tests`]: a fresh signed-in
//! session plus the request builders both files' tests send it through.
use std::path::Path;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Request;
use axum::http::header;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tower::ServiceExt;

use super::{NetworkAccess, Response, Router, SettingsServer, StatusCode, random_hex, test_keychain};
use crate::env::Env;
use crate::store::MachineStore;

pub(crate) const HOST: &str = "127.0.0.1:4700";

pub(crate) fn app(settings: Arc<SettingsServer>) -> Router {
    super::router(settings).layer(MockConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))))
}

pub(crate) fn request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder().method(method).uri(path).header("Host", HOST)
}

pub(crate) async fn json_body(res: Response) -> Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// Signs in and `GET /api/settings`, for a test that only cares what comes back.
pub(crate) async fn signed_in_settings(settings: &Arc<SettingsServer>) -> Value {
    let app = app(settings.clone());
    let (cookie, _csrf) = sign_in(&app, settings).await;
    let res = app
        .oneshot(
            request("GET", "/api/settings")
                .header("Cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await
}

pub(crate) fn mint_token(settings: &SettingsServer, good_for_secs: u64) -> String {
    let token = random_hex(16);
    let expires_at = (settings.now)() + good_for_secs;
    settings.state.lock().unwrap().tokens.insert(token.clone(), expires_at);
    token
}

pub(crate) fn post_body(path: &str, body: &str) -> Request<Body> {
    request("POST", path)
        .header("Origin", format!("http://{HOST}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub(crate) async fn sign_in(app: &Router, settings: &SettingsServer) -> (String, String) {
    let token = mint_token(settings, 300);
    let res = app
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": token }).to_string(),
        ))
        .await
        .unwrap();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let cookie = cookie.split(';').next().unwrap().to_string();
    let csrf = json_body(res).await["csrfToken"].as_str().unwrap().to_string();
    (cookie, csrf)
}

/// Starts a signed-in session against a fresh config dir, for a test that posts a setting.
pub(crate) async fn signed_in_app(dir: &Path) -> (Arc<SettingsServer>, Router, String, String) {
    let settings = settings_with_network(dir, false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    (settings, app, cookie, csrf)
}

pub(crate) fn settings_with_network(
    dir: &Path,
    network_enabled: bool,
    stored: Option<NetworkAccess>,
) -> Arc<SettingsServer> {
    if let Some(network) = stored {
        MachineStore::load(dir).unwrap().set_network(network).unwrap();
    }
    Arc::new(SettingsServer::new(
        dir.to_path_buf(),
        4700,
        4700,
        network_enabled,
        Env::empty(),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir),
    ))
}

pub(crate) async fn post_setting(app: &Router, cookie: &str, csrf: &str, body: Value) -> Response {
    app.clone()
        .oneshot(
            request("POST", "/api/settings")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie.to_string())
                .header("X-CSRF-Token", csrf.to_string())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}
