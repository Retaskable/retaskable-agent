//! The CI runner's own settings page: its port and address, what it serves, and that it signs in and saves with the same
//! guarantees as the code agent's page (the auth core's own tests are `page_host`'s). Temp data folders, an injected
//! clock and a fake credential store; no real keychain and no listening socket.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::page_host::TOKEN_TTL_SECS;
use crate::program_settings::ci as ci_file;
use crate::program_status::{Linked, Report, StatusWriter};
use crate::secrets::fakes::MemStore;

const HOST: &str = "127.0.0.1:4702";
const NOW_MS: u64 = 1_700_000_000_000;
const TOKEN: &str = "ghp_PLANTED_ci_token_1234567890";
const PASSWORD: &str = "a long enough one";

struct Rig {
    root: tempfile::TempDir,
    host: Arc<PageHost>,
    clock: Arc<AtomicU64>,
}

impl Rig {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// A page over a fresh data folder; `tweak` runs before the host is shared (own addresses and the like).
    fn with(tweak: impl FnOnce(&mut PageHost)) -> Self {
        let root = tempfile::tempdir().unwrap();
        let clock = Arc::new(AtomicU64::new(NOW_MS / 1000));
        let c = clock.clone();
        let mut host = host(&Env::empty(), root.path(), 4702, move || c.load(Ordering::SeqCst)).unwrap();
        host.set_program_keys(Some(Arc::new(MemStore::default())));
        tweak(Arc::get_mut(&mut host).unwrap());
        Self { root, host, clock }
    }

    fn ci_dir(&self) -> std::path::PathBuf {
        crate::paths::ci_dir(self.root.path())
    }

    fn app_from(&self, peer: IpAddr) -> Router {
        router(self.host.clone(), "1.2.3").layer(MockConnectInfo(SocketAddr::from((peer, 54321))))
    }

    fn app(&self) -> Router {
        self.app_from(Ipv4Addr::LOCALHOST.into())
    }

    /// The token in the link the page prints at start.
    fn link_token(&self) -> String {
        self.host.settings_link().split_once('#').unwrap().1.to_string()
    }

    fn network(&self, on: bool) {
        ci_file::set_password(&self.ci_dir(), PASSWORD).unwrap();
        ci_file::set_network(&self.ci_dir(), on).unwrap();
    }
}

fn get(path: &str) -> axum::http::request::Builder {
    Request::builder().method("GET").uri(path).header("Host", HOST)
}

fn post(path: &str, host: &str) -> axum::http::request::Builder {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("Host", host)
        .header("Origin", format!("http://{host}"))
}

async fn body_json(res: axum::response::Response) -> Value {
    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn sign_in(app: &Router, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            post("/api/settings/session", HOST)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// `(cookie, csrf)` from signing in with the printed link's token.
async fn signed_in(rig: &Rig, app: &Router) -> (String, String) {
    let res = sign_in(app, json!({ "token": rig.link_token() })).await;
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = cookie.split(';').next().unwrap().to_string();
    (cookie, body_json(res).await["csrfToken"].as_str().unwrap().to_string())
}

async fn save(app: &Router, cookie: &str, csrf: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            post("/api/settings/programs/ci-runner", HOST)
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn status_file(rig: &Rig) {
    StatusWriter::new(&rig.ci_dir(), crate::program_status::CI_RUNNER, "1.2.3", NOW_MS)
        .write(
            NOW_MS,
            Report {
                linked: Linked {
                    store: true,
                    cloud: None,
                },
                ..Report::default()
            },
        )
        .unwrap();
}

#[test]
fn the_port_is_the_flag_then_the_variable_then_4702_and_a_bad_one_is_an_error() {
    let none = Env::empty();
    let var = Env::from_pairs([(PORT_VAR, "5000")]);
    assert_eq!(port(&none, None).unwrap(), DEFAULT_PORT);
    assert_eq!(port(&var, None).unwrap(), 5000);
    assert_eq!(port(&var, Some("6000")).unwrap(), 6000);
    for bad in ["80", "1023", "70000", "abc", "", "-1"] {
        assert!(port(&none, Some(bad)).is_err(), "{bad}");
    }
    assert!(port(&Env::from_pairs([(PORT_VAR, "x")]), None).is_err());
}

#[test]
fn it_listens_on_this_computer_only_unless_network_access_is_on() {
    assert_eq!(bind_addr(false, &Env::empty(), 4702).to_string(), "127.0.0.1:4702");
    assert_eq!(bind_addr(true, &Env::empty(), 4702).to_string(), "0.0.0.0:4702");
}

#[tokio::test]
async fn it_serves_the_health_check_and_the_trimmed_page_and_nothing_else() {
    let rig = Rig::new();
    let res = rig
        .app()
        .oneshot(get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(body_json(res).await, json!({ "ok": true, "version": "1.2.3" }));
    let res = rig
        .app()
        .oneshot(get("/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
    let html = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let nonce = csp.split("'nonce-").nth(1).unwrap().split('\'').next().unwrap();
    assert!(!html.contains("{{"), "every placeholder is filled");
    assert!(html.contains("RT.programs = { load: load }") && html.contains("<title>CI runner</title>"));
    // The code agent's page has none of its own sections here.
    assert!(!html.contains("lane-cards") && !html.contains("/api/settings/status") && !html.contains("/api/profile"));
    assert_eq!(html.matches(&format!("<script nonce=\"{nonce}\">")).count(), 3);
    for (method, path) in [
        ("GET", "/api/settings"),
        ("GET", "/api/settings/status"),
        ("POST", "/api/settings"),
        ("POST", "/api/settings/restart"),
        ("GET", "/api/link"),
        ("POST", "/api/health"),
        ("GET", "/"),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("Host", HOST)
            .header("Origin", format!("http://{HOST}"))
            .body(Body::empty())
            .unwrap();
        let res = rig.app().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
}

#[tokio::test]
async fn the_printed_link_signs_in_once_and_only_from_this_computer() {
    let rig = Rig::new();
    let token = rig.link_token();
    // Another computer's try is refused and doesn't use the token up (network access is on, so it gets that far).
    rig.network(true);
    let own: IpAddr = "192.168.1.20".parse().unwrap();
    let rig2 = Rig::with(|h| h.own_addresses = Box::new(move || vec![own]));
    rig2.network(true);
    let token2 = rig2.link_token();
    let remote = rig2.app_from("192.168.1.30".parse().unwrap());
    let res = remote
        .oneshot(
            post("/api/settings/session", "192.168.1.20:4702")
                .body(Body::from(json!({ "token": token2 }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        sign_in(&rig2.app(), json!({ "token": token2 })).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        sign_in(&rig2.app(), json!({ "token": token2 })).await.status(),
        StatusCode::UNAUTHORIZED
    );
    // A token is good for five minutes and no longer.
    rig.clock.fetch_add(TOKEN_TTL_SECS, Ordering::SeqCst);
    assert_eq!(
        sign_in(&rig.app(), json!({ "token": token })).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_password_signs_in_from_this_computer_always_and_from_another_only_with_network_access_on() {
    let rig = Rig::new();
    let attempt = |p: &str| sign_in_with(&rig, json!({ "password": p }));
    let remote: IpAddr = "192.168.1.30".parse().unwrap();
    // No password set: none signs in.
    assert_eq!(attempt(PASSWORD).await, StatusCode::UNAUTHORIZED);
    // Set, with network access off (the default): this computer signs in with it, another never does.
    rig.network(false);
    assert_eq!(attempt("wrong password!").await, StatusCode::UNAUTHORIZED);
    assert_eq!(attempt(PASSWORD).await, StatusCode::OK);
    assert!(!rig.host.verify_password(PASSWORD, remote));
    // On: another computer signs in with it too, and `network off` takes effect at once, without a restart.
    ci_file::set_network(&rig.ci_dir(), true).unwrap();
    assert!(rig.host.verify_password(PASSWORD, remote));
    ci_file::set_network(&rig.ci_dir(), false).unwrap();
    assert!(!rig.host.verify_password(PASSWORD, remote));
    // A settings file that can't be read signs nobody in (fail closed), not even this computer.
    std::fs::write(ci_file_path(&rig), "{nope").unwrap();
    assert_eq!(attempt(PASSWORD).await, StatusCode::UNAUTHORIZED);
}

fn ci_file_path(rig: &Rig) -> std::path::PathBuf {
    crate::program_settings::file_in(&rig.ci_dir())
}

async fn sign_in_with(rig: &Rig, body: Value) -> StatusCode {
    sign_in(&rig.app(), body).await.status()
}

#[tokio::test]
async fn five_wrong_passwords_make_even_the_right_one_wait() {
    let rig = Rig::new();
    rig.network(false);
    for _ in 0..5 {
        assert_eq!(
            sign_in_with(&rig, json!({ "password": "nope nope nope" })).await,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        sign_in_with(&rig, json!({ "password": PASSWORD })).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    rig.clock.fetch_add(61, Ordering::SeqCst);
    assert_eq!(
        sign_in_with(&rig, json!({ "password": PASSWORD })).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_save_needs_a_session_the_csrf_token_this_hosts_origin_and_a_small_body() {
    let rig = Rig::new();
    status_file(&rig);
    let app = rig.app();
    let body = json!({ "settings": { "slots": 3 } });
    // No session.
    assert_eq!(
        save(&app, "", "x", body.clone()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let (cookie, csrf) = signed_in(&rig, &app).await;
    // A session without the CSRF token, or with a wrong one.
    assert_eq!(
        save(&app, &cookie, "", body.clone()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        save(&app, &cookie, "wrong", body.clone()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    // Another page's Origin, and a Host that is not this computer's: the same 404 as an unknown path.
    let foreign = Request::builder()
        .method("POST")
        .uri("/api/settings/programs/ci-runner")
        .header("Host", HOST)
        .header("Origin", "http://evil.example")
        .header("Cookie", cookie.clone())
        .header("X-CSRF-Token", csrf.clone())
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(foreign).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let rebinding = Request::builder()
        .method("POST")
        .uri("/api/settings/programs/ci-runner")
        .header("Host", "evil.example:4702")
        .header("Origin", "http://evil.example:4702")
        .header("Cookie", cookie.clone())
        .header("X-CSRF-Token", csrf.clone())
        .body(Body::from(body.to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(rebinding).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    // Over 16 KB.
    let big = json!({ "settings": { "name": "x".repeat(17 * 1024) } });
    assert_eq!(
        save(&app, &cookie, &csrf, big).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(!ci_file_path(&rig).exists(), "nothing above wrote the file");
    // And the good one.
    assert_eq!(save(&app, &cookie, &csrf, body).await.status(), StatusCode::OK);
    assert_eq!(ci_file::read(&rig.ci_dir()).unwrap().slots, Some(3));
}

#[tokio::test]
async fn a_save_leaves_the_network_key_and_the_page_never_shows_it() {
    let rig = Rig::new();
    status_file(&rig);
    rig.network(true);
    let app = rig.app();
    let (cookie, csrf) = signed_in(&rig, &app).await;
    let res = save(&app, &cookie, &csrf, json!({ "settings": { "slots": 4 } })).await;
    assert_eq!(res.status(), StatusCode::OK);
    let kept = ci_file::network(&rig.ci_dir()).unwrap();
    assert!(kept.enabled && kept.password_hash.is_some());
    // The page can't set it: it isn't a field of the runner.
    let res = save(
        &app,
        &cookie,
        &csrf,
        json!({ "settings": { "network": { "enabled": false } } }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(ci_file::network(&rig.ci_dir()).unwrap().enabled);
    let res = app
        .clone()
        .oneshot(
            get("/api/settings/programs")
                .header("Cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let text = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(!text.contains("scrypt") && !text.contains("passwordHash"));
}

#[tokio::test]
async fn a_token_is_saved_only_from_this_computer_and_never_comes_back() {
    let own: IpAddr = "192.168.1.20".parse().unwrap();
    let rig = Rig::with(|h| h.own_addresses = Box::new(move || vec![own]));
    status_file(&rig);
    let app = rig.app();
    let (cookie, csrf) = signed_in(&rig, &app).await;
    let res = save(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": TOKEN } })).await;
    assert_eq!(res.status(), StatusCode::OK);
    let all = {
        let res = app
            .clone()
            .oneshot(
                get("/api/settings/programs")
                    .header("Cookie", cookie.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    };
    assert!(!all.contains(TOKEN), "a secret is write-only");
    assert!(
        !std::fs::read_to_string(ci_file_path(&rig)).unwrap().contains(TOKEN),
        "and sealed on disk"
    );
    let value: Value = serde_json::from_str(&all).unwrap();
    let ci = value["programs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "ci-runner")
        .unwrap();
    assert_eq!(ci["canSaveSecrets"], true);
    assert_eq!(ci["secrets"][0]["source"], "saved");
    assert!(ci["secrets"][0]["fingerprint"].is_string());
    // Signed in over the network with the password, the same save is refused and nothing is written.
    rig.network(true);
    let remote = rig.app_from("192.168.1.30".parse().unwrap());
    let res = remote
        .clone()
        .oneshot(
            post("/api/settings/session", "192.168.1.20:4702")
                .body(Body::from(json!({ "password": PASSWORD }).to_string()))
                .unwrap(),
        )
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
    let csrf = body_json(res).await["csrfToken"].as_str().unwrap().to_string();
    let before = std::fs::read_to_string(ci_file_path(&rig)).unwrap();
    let res = remote
        .oneshot(
            post("/api/settings/programs/ci-runner", "192.168.1.20:4702")
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::from(json!({ "secrets": { "GH_TOKEN": null } }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(std::fs::read_to_string(ci_file_path(&rig)).unwrap(), before);
}

#[tokio::test]
async fn another_computer_gets_a_404_until_network_access_is_on() {
    let own: IpAddr = "192.168.1.20".parse().unwrap();
    let rig = Rig::with(|h| h.own_addresses = Box::new(move || vec![own]));
    let remote = || rig.app_from("192.168.1.30".parse().unwrap());
    let request = || {
        Request::builder()
            .uri("/settings")
            .header("Host", "192.168.1.20:4702")
            .body(Body::empty())
            .unwrap()
    };
    // Off, with no key at all.
    assert_eq!(
        remote().oneshot(request()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    // Its own address as the Host from this computer is refused too while access is off (no DNS rebinding either).
    let local = rig.app().oneshot(request()).await.unwrap();
    assert_eq!(local.status(), StatusCode::NOT_FOUND);
    rig.network(true);
    assert_eq!(remote().oneshot(request()).await.unwrap().status(), StatusCode::OK);
    ci_file::set_network(&rig.ci_dir(), false).unwrap();
    assert_eq!(
        remote().oneshot(request()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn a_piped_password_is_its_first_line_with_its_spaces() {
    assert_eq!(password_from_line("  two spaces \r\n"), "  two spaces ");
    assert_eq!(password_from_line("plain"), "plain");
}

#[test]
fn the_commands_write_the_runners_own_file_and_make_its_folder() {
    let root = tempfile::tempdir().unwrap();
    let env = Env::from_pairs([("RETASKABLE_DATA_DIR", root.path().to_str().unwrap())]);
    assert!(set_network(&env, true).is_err(), "on needs a password");
    assert!(set_password(&env, "short").is_err());
    set_password(&env, PASSWORD).unwrap();
    set_network(&env, true).unwrap();
    let dir = Path::new(root.path()).join(crate::paths::CI_SUBDIR);
    assert!(ci_file::network(&dir).unwrap().enabled);
}

const GATEWAY: &str = "172.17.0.1";

#[tokio::test]
async fn the_docker_gateway_is_this_computer_only_with_the_opt_in_and_network_access_off() {
    // With the opt-in the gateway signs in with the link, and saves a token, as loopback does.
    let rig = Rig::with(|h| h.trusted_gateway = Some(GATEWAY.parse().unwrap()));
    status_file(&rig);
    let app = rig.app_from(GATEWAY.parse().unwrap());
    let (cookie, csrf) = signed_in(&rig, &app).await;
    let res = save(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": TOKEN } })).await;
    assert_eq!(res.status(), StatusCode::OK);
    // And the password signs in from it with network access off, as from loopback (`CiAccess`).
    rig.network(false);
    assert_eq!(
        sign_in(&app, json!({ "password": PASSWORD })).await.status(),
        StatusCode::OK
    );
    // Another peer is refused as before, the page not even found.
    let other = rig.app_from("172.17.0.9".parse().unwrap());
    let res = other
        .oneshot(get("/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn without_the_opt_in_the_gateway_is_refused_like_any_other_computer() {
    let rig = Rig::new();
    let app = rig.app_from(GATEWAY.parse().unwrap());
    let res = app
        .oneshot(get("/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
