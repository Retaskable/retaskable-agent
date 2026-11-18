//! The Link button on the CI runner's own trimmed page (agent-settings.md section 4.6, agent-ci.md section 3.9): the same
//! route and flow as the code agent's page, run by `retaskable-ci` itself, writing its own token file.
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::link::fake_cloud::{self, REFRESH, USER_CODE};

const HOST: &str = "127.0.0.1:4702";

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_trimmed_page_links_the_runner_to_the_store_and_writes_its_own_token_file() {
    let root = tempfile::tempdir().unwrap();
    let store = fake_cloud::spawn().await;
    let file = crate::paths::ci_dir(root.path()).join("credentials.json");
    let env = Env::from_pairs([
        ("RETASKABLE_BACKLOG_URL", store.base()),
        ("RETASKABLE_CI_CREDENTIALS_FILE", file.to_string_lossy().to_string()),
    ]);
    let host = host(&env, root.path(), 4702, || crate::program_status::now_ms() / 1000).unwrap();
    host.set_version("1.2.3");
    host.program_links.set_second(Duration::from_millis(1));
    let app = router(host.clone(), "1.2.3").layer(MockConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))));

    let token = host.settings_link().split_once('#').unwrap().1.to_string();
    let session = Request::builder()
        .method("POST")
        .uri("/api/settings/session")
        .header("Host", HOST)
        .header("Origin", format!("http://{HOST}"))
        .body(Body::from(json!({ "token": token }).to_string()))
        .unwrap();
    let (status, headers, body) = send(&app, session).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = body["csrfToken"].as_str().unwrap().to_string();

    // Without the CSRF token it starts nothing.
    let link = |csrf: Option<&str>| {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/settings/programs/ci-runner/link")
            .header("Host", HOST)
            .header("Origin", format!("http://{HOST}"))
            .header("Cookie", cookie.clone());
        if let Some(c) = csrf {
            req = req.header("X-CSRF-Token", c.to_string());
        }
        req.body(Body::empty()).unwrap()
    };
    assert_eq!(send(&app, link(None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(store.asked(), 0);
    assert_eq!(send(&app, link(Some(&csrf))).await.2, json!({ "linking": true }));

    let programs = || {
        Request::builder()
            .method("GET")
            .uri("/api/settings/programs")
            .header("Host", HOST)
            .header("Cookie", cookie.clone())
            .body(Body::empty())
            .unwrap()
    };
    let mut changes = host.program_links.subscribe();
    let mut shown = String::new();
    loop {
        let (_, _, body) = send(&app, programs()).await;
        shown.push_str(&body.to_string());
        let ci = body["programs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "ci-runner")
            .unwrap()
            .clone();
        if ci["link"]["approve"]["code"] == json!(USER_CODE) && !store.codes.lock().unwrap().is_empty() {
            store.approve();
        }
        if ci["link"]["linked"] == json!(true) {
            break;
        }
        tokio::time::timeout(Duration::from_secs(10), changes.changed())
            .await
            .unwrap()
            .unwrap();
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["retaskable-ci"][store.base()], json!(REFRESH));
    assert!(!shown.contains(REFRESH) && !shown.contains(fake_cloud::DEVICE_CODE));
    assert_eq!(store.codes.lock().unwrap()[0]["role"], json!("ci"));
}
