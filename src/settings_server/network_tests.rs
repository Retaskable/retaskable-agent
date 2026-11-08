//! Other computers, and this computer's own network addresses as Host, only while network access
//! is on (agent-settings.md sections 4.3 and 4.4).
use axum::body::Body;
use axum::http::Request;
use serde_json::json;
use std::net::IpAddr;
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::password;

const PASSWORD: &str = "a good enough password";
const PEER: &str = "192.168.1.50";
const OWN: &str = "192.168.1.20";

fn on() -> Option<NetworkAccess> {
    Some(NetworkAccess {
        enabled: true,
        password_hash: Some(password::hash_password(PASSWORD)),
    })
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

async fn status_of(app: &Router, method: &str, path: &str) -> StatusCode {
    let req = if method == "POST" {
        post_body(path, "{}")
    } else {
        request(method, path).body(Body::empty()).unwrap()
    };
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn with_network_access_off_another_computer_gets_a_404_everywhere_and_loopback_works() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_own_addresses(dir.path(), None, vec![]);
    let remote = app_from(settings.clone(), ip(PEER));
    for path in ["/settings", "/api/settings", "/api/settings/status"] {
        assert_eq!(status_of(&remote, "GET", path).await, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        status_of(&remote, "POST", "/api/settings/session").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(status_of(&app(settings), "GET", "/settings").await, StatusCode::OK);
}

#[tokio::test]
async fn with_network_access_on_another_computer_signs_in_with_the_password_not_a_tray_token() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_own_addresses(dir.path(), on(), vec![]);
    let remote = app_from(settings.clone(), ip(PEER));
    assert_eq!(status_of(&remote, "GET", "/settings").await, StatusCode::OK);

    let token = mint_token(&settings, 300);
    let with_token = remote
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": token }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(with_token.status(), StatusCode::UNAUTHORIZED);
    assert!(
        settings.state.lock().unwrap().tokens.contains_key(&token),
        "not used up"
    );

    let with_password = remote
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": PASSWORD }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(with_password.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_environment_password_turns_network_access_on_for_other_computers() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        true,
        Env::from_pairs([("RETASKABLE_NETWORK_PASSWORD", PASSWORD)]),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    ));
    assert_eq!(
        status_of(&app_from(settings, ip(PEER)), "GET", "/settings").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_save_that_turns_access_off_locks_other_computers_out_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_own_addresses(dir.path(), on(), vec![]);
    let (cookie, csrf) = sign_in(&app(settings.clone()), &settings).await;
    let remote = app_from(settings.clone(), ip(PEER));
    assert_eq!(status_of(&remote, "GET", "/settings").await, StatusCode::OK);

    let saved = post_setting(
        &app(settings),
        &cookie,
        &csrf,
        json!({ "network": { "enabled": false } }),
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(status_of(&remote, "GET", "/settings").await, StatusCode::NOT_FOUND);
}

fn get_with_host(host: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/settings")
        .header("Host", host)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn an_own_network_address_is_a_host_only_while_access_is_on_and_at_the_right_port() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_own_addresses(dir.path(), on(), vec![ip(OWN), ip("fe80::1")]);
    let remote = app_from(settings, ip(PEER));
    for (host, expected) in [
        ("192.168.1.20:4700", StatusCode::OK),
        ("[fe80::1]:4700", StatusCode::OK),
        ("192.168.1.20:4701", StatusCode::NOT_FOUND),
        ("192.168.1.21:4700", StatusCode::NOT_FOUND),
        ("evil.example.com:4700", StatusCode::NOT_FOUND),
    ] {
        let res = remote.clone().oneshot(get_with_host(host)).await.unwrap();
        assert_eq!(res.status(), expected, "{host}");
    }

    let off_dir = tempfile::tempdir().unwrap();
    let off = settings_with_own_addresses(off_dir.path(), None, vec![ip(OWN)]);
    let res = app(off).oneshot(get_with_host("192.168.1.20:4700")).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_post_from_an_own_network_address_needs_that_host_as_its_origin() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_own_addresses(dir.path(), on(), vec![ip(OWN)]);
    let remote = app_from(settings, ip(PEER));
    let body = json!({ "password": PASSWORD }).to_string();
    let post = |origin: &str| {
        Request::builder()
            .method("POST")
            .uri("/api/settings/session")
            .header("Host", "192.168.1.20:4700")
            .header("Origin", origin.to_string())
            .header("Content-Type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };
    let wrong = remote.clone().oneshot(post("http://127.0.0.1:4700")).await.unwrap();
    assert_eq!(wrong.status(), StatusCode::NOT_FOUND);
    let right = remote.oneshot(post("http://192.168.1.20:4700")).await.unwrap();
    assert_eq!(right.status(), StatusCode::OK);
}
