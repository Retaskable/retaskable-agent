//! Other computers, and Host as this computer's own network address, only while network access
//! is on (agent-settings.md sections 4.3 and 4.4).
use std::net::IpAddr;

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::password;

const PW: &str = "a good enough password";
const PEER: &str = "192.168.1.50";
const OWN4: &str = "192.168.1.20";
const OWN6: &str = "fd00::20";

fn on() -> Option<NetworkAccess> {
    Some(NetworkAccess {
        enabled: true,
        password_hash: Some(password::hash_password(PW)),
    })
}

fn off() -> Option<NetworkAccess> {
    Some(NetworkAccess {
        enabled: false,
        password_hash: Some(password::hash_password(PW)),
    })
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn server(dir: &std::path::Path, stored: Option<NetworkAccess>) -> Arc<SettingsServer> {
    let mut s = Arc::try_unwrap(settings_with_network(dir, true, stored)).ok().unwrap();
    set_own_addresses(&mut s, vec![ip(OWN4), ip(OWN6)]);
    Arc::new(s)
}

async fn status_of(app: &Router, method: &str, path: &str, host: &str) -> StatusCode {
    let mut req = Request::builder().method(method).uri(path).header("Host", host);
    if method == "POST" {
        req = req.header("Origin", format!("http://{host}"));
    }
    app.clone()
        .oneshot(req.body(Body::from("{}")).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn with_network_access_off_another_computer_gets_the_404_on_every_path() {
    let dir = tempfile::tempdir().unwrap();
    let settings = server(dir.path(), off());
    let remote = app_from(settings.clone(), ip(PEER));
    for (m, p) in [
        ("GET", "/settings"),
        ("GET", "/api/settings"),
        ("POST", "/api/settings/session"),
        ("GET", "/api/settings/status"),
    ] {
        assert_eq!(status_of(&remote, m, p, HOST).await, StatusCode::NOT_FOUND, "{p}");
    }
    let local = app(settings);
    assert_eq!(status_of(&local, "GET", "/settings", HOST).await, StatusCode::OK);
}

#[tokio::test]
async fn with_network_access_on_another_computer_signs_in_with_the_password_only() {
    let dir = tempfile::tempdir().unwrap();
    let settings = server(dir.path(), on());
    let remote = app_from(settings.clone(), ip(PEER));
    let host = format!("{OWN4}:4700");
    assert_eq!(status_of(&remote, "GET", "/settings", &host).await, StatusCode::OK);

    let token = mint_token(&settings, 300);
    let res = remote
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/session")
                .header("Host", &host)
                .header("Origin", format!("http://{host}"))
                .body(Body::from(json!({ "token": token }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(
        settings.state.lock().unwrap().tokens.contains_key(&token),
        "not used up"
    );

    let res = remote
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/session")
                .header("Host", &host)
                .header("Origin", format!("http://{host}"))
                .body(Body::from(json!({ "password": PW }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_env_password_turns_network_access_on_for_another_computer() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::from_pairs([("RETASKABLE_NETWORK_PASSWORD", PW)]);
    let mut s = SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        true,
        env,
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    );
    set_own_addresses(&mut s, vec![]);
    let remote = app_from(Arc::new(s), ip(PEER));
    assert_eq!(status_of(&remote, "GET", "/settings", HOST).await, StatusCode::OK);
}

#[tokio::test]
async fn a_save_turning_access_off_locks_another_computer_out_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let settings = server(dir.path(), on());
    let remote = app_from(settings.clone(), ip(PEER));
    let res = remote
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": PW }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = json_body(res).await["csrfToken"].as_str().unwrap().to_string();
    let saved = post_setting(&remote, &cookie, &csrf, json!({ "network": { "enabled": false } })).await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(
        status_of(&remote, "GET", "/settings", HOST).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn an_own_network_address_is_a_host_only_while_access_is_on_at_the_right_port() {
    let dir = tempfile::tempdir().unwrap();
    let settings = server(dir.path(), on());
    let remote = app_from(settings.clone(), ip(PEER));
    for host in [format!("{OWN4}:4700"), format!("[{OWN6}]:4700")] {
        assert_eq!(
            status_of(&remote, "GET", "/settings", &host).await,
            StatusCode::OK,
            "{host}"
        );
    }
    for host in [
        format!("{OWN4}:9999"),
        "192.168.1.99:4700".to_string(),
        "evil.example.com:4700".to_string(),
    ] {
        assert_eq!(
            status_of(&remote, "GET", "/settings", &host).await,
            StatusCode::NOT_FOUND,
            "{host}"
        );
    }
    // A POST from an own address needs Origin http://<that host>.
    let host = format!("{OWN4}:4700");
    let bad = Request::builder()
        .method("POST")
        .uri("/api/settings/session")
        .header("Host", &host)
        .header("Origin", "http://evil.example.com")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(remote.oneshot(bad).await.unwrap().status(), StatusCode::NOT_FOUND);

    let dir = tempfile::tempdir().unwrap();
    let off_app = app(server(dir.path(), off()));
    assert_eq!(
        status_of(&off_app, "GET", "/settings", &host).await,
        StatusCode::NOT_FOUND
    );
}
