use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

use super::*;
use crate::config::ConfigFile;

/// A fake cloud's device endpoints: each POST to `/token` takes the next answer, and every body is kept.
#[derive(Default)]
struct Fake {
    code: StdMutex<Option<(StatusCode, Value)>>,
    token: StdMutex<VecDeque<(StatusCode, Value)>>,
    bodies: StdMutex<Vec<Value>>,
}

async fn serve(fake: Arc<Fake>) -> String {
    async fn code(State(f): State<Arc<Fake>>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        f.bodies.lock().unwrap().push(body);
        let (status, json) = f.code.lock().unwrap().clone().unwrap();
        (status, Json(json))
    }
    async fn token(State(f): State<Arc<Fake>>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        f.bodies.lock().unwrap().push(body);
        let (status, json) = f.token.lock().unwrap().pop_front().expect("an answer for /token");
        (status, Json(json))
    }
    let app = Router::new()
        .route("/api/device/code", post(code))
        .route("/api/device/token", post(token))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn store() -> (tempfile::TempDir, Arc<dyn TokenStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("credentials.json"), "retaskable"));
    (dir, store)
}

fn link(store: &dyn TokenStore) -> Link<'_> {
    Link {
        http: reqwest::Client::new(),
        store,
        machine: json!({ "machine": "pc", "os": "Windows", "agentVersion": "0.1.0" }),
        target: CLOUD_TARGET,
        second: Duration::from_millis(1),
    }
}

fn pending(error: &str) -> (StatusCode, Value) {
    (StatusCode::BAD_REQUEST, json!({ "error": error }))
}

#[tokio::test]
async fn a_link_shows_its_code_polls_until_approved_and_keeps_the_refresh_token() {
    let fake = Arc::new(Fake::default());
    *fake.code.lock().unwrap() = Some((
        StatusCode::OK,
        json!({ "device_code": "dc", "user_code": "WXYZ-2345", "verification_uri": "https://x/link",
                "verification_uri_complete": "https://x/link?code=WXYZ-2345", "interval": 1, "expires_in": 900 }),
    ));
    fake.token.lock().unwrap().extend([
        pending("authorization_pending"),
        pending("slow_down"),
        (StatusCode::OK, json!({ "refresh_token": "r1", "agent_id": 7 })),
    ]);
    let url = serve(fake.clone()).await;
    let (_dir, store) = store();
    let mut shown = None;
    let agent = link(store.as_ref())
        .run(&url, |c| shown = Some(c.clone()))
        .await
        .unwrap();
    assert_eq!(agent, 7);
    let shown = shown.unwrap();
    assert_eq!(shown.user_code, "WXYZ-2345");
    assert_eq!(shown.verification_uri_complete, "https://x/link?code=WXYZ-2345");
    assert_eq!(shown.expires_in, 900);
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r1"));
    let bodies = fake.bodies.lock().unwrap();
    assert_eq!(bodies[0]["machine"], "pc");
    assert_eq!(
        bodies[1],
        json!({ "grant_type": DEVICE_CODE_GRANT, "device_code": "dc" })
    );
    assert_eq!(bodies.len(), 4);
}

#[tokio::test]
async fn a_denied_or_expired_link_says_so() {
    for (error, says) in [
        ("access_denied", "denied"),
        ("expired_token", "expired"),
        ("weird", "Linking failed: weird"),
    ] {
        let fake = Arc::new(Fake::default());
        *fake.code.lock().unwrap() = Some((StatusCode::OK, json!({ "device_code": "dc", "interval": 1 })));
        fake.token.lock().unwrap().push_back(pending(error));
        let url = serve(fake).await;
        let (_dir, store) = store();
        let err = link(store.as_ref()).run(&url, |_| {}).await.unwrap_err();
        assert!(err.to_string().contains(says), "{err}");
        assert_eq!(store.get(&url).unwrap(), None);
    }
}

#[tokio::test]
async fn a_refused_code_and_an_unreachable_cloud_say_why() {
    let fake = Arc::new(Fake::default());
    *fake.code.lock().unwrap() = Some((
        StatusCode::TOO_MANY_REQUESTS,
        json!({ "error": "slow", "error_description": "Too many codes" }),
    ));
    let url = serve(fake).await;
    let (_dir, store) = store();
    let err = link(store.as_ref()).run(&url, |_| {}).await.unwrap_err();
    assert_eq!(err.to_string(), "Couldn't get a code: Too many codes");

    // Nothing listens on port 9 of localhost.
    let err = link(store.as_ref())
        .run("http://127.0.0.1:9", |_| {})
        .await
        .unwrap_err();
    assert!(
        err.to_string().starts_with("Couldn't reach http://127.0.0.1:9: "),
        "{err}"
    );
    assert!(
        err.to_string().ends_with("Check cloud.url or RETASKABLE_CLOUD_URL."),
        "{err}"
    );
}

fn tokens(url: &str, store: Arc<dyn TokenStore>) -> DeviceTokens {
    DeviceTokens::new(url.to_string(), CLOUD_TARGET, store, reqwest::Client::new())
}

#[tokio::test]
async fn an_unlinked_cloud_is_idle_without_asking_it() {
    let (_dir, store) = store();
    // Nothing listens here: asking would answer Unreachable, not Idle.
    assert_eq!(
        tokens("http://127.0.0.1:9", store).access_token(None).await,
        TokenAnswer::Idle
    );
}

#[tokio::test]
async fn access_tokens_are_kept_until_stale_and_each_refresh_keeps_the_rotated_token() {
    let fake = Arc::new(Fake::default());
    fake.token.lock().unwrap().extend([
        (
            StatusCode::OK,
            json!({ "access_token": "a1", "refresh_token": "r2", "expires_in": 3600 }),
        ),
        (
            StatusCode::OK,
            json!({ "access_token": "a2", "refresh_token": "r3", "expires_in": 3600 }),
        ),
    ]);
    let url = serve(fake.clone()).await;
    let (_dir, store) = store();
    store.set(&url, "r1").unwrap();
    let t = tokens(&url, store.clone());
    let token = |a: &str| TokenAnswer::Token {
        url: url.clone(),
        access_token: a.to_string(),
    };
    assert_eq!(t.access_token(None).await, token("a1"));
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r2"));
    // Kept: no second refresh.
    assert_eq!(t.access_token(None).await, token("a1"));
    // A token the caller found no good isn't handed out again.
    assert_eq!(t.access_token(Some("a1")).await, token("a2"));
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r3"));
    let bodies = fake.bodies.lock().unwrap();
    assert_eq!(
        bodies[0],
        json!({ "grant_type": "refresh_token", "refresh_token": "r1" })
    );
    assert_eq!(bodies[1]["refresh_token"], "r2");
}

#[tokio::test]
async fn a_token_about_to_expire_is_refreshed() {
    let fake = Arc::new(Fake::default());
    fake.token.lock().unwrap().extend([
        (
            StatusCode::OK,
            json!({ "access_token": "a1", "refresh_token": "r2", "expires_in": 30 }),
        ),
        (
            StatusCode::OK,
            json!({ "access_token": "a2", "refresh_token": "r3", "expires_in": 3600 }),
        ),
    ]);
    let url = serve(fake).await;
    let (_dir, store) = store();
    store.set(&url, "r1").unwrap();
    let t = tokens(&url, store);
    assert!(matches!(t.access_token(None).await, TokenAnswer::Token { access_token, .. } if access_token == "a1"));
    // Under a minute left: refreshed at once.
    assert!(matches!(t.access_token(None).await, TokenAnswer::Token { access_token, .. } if access_token == "a2"));
}

#[tokio::test]
async fn invalid_grant_deletes_the_stored_token_and_goes_idle() {
    let fake = Arc::new(Fake::default());
    fake.token.lock().unwrap().push_back((
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_grant", "error_description": "unlinked" }),
    ));
    let url = serve(fake).await;
    let (_dir, store) = store();
    store.set(&url, "r1").unwrap();
    assert_eq!(tokens(&url, store.clone()).access_token(None).await, TokenAnswer::Idle);
    assert_eq!(store.get(&url).unwrap(), None);
}

#[tokio::test]
async fn another_failure_keeps_the_stored_token_and_reads_as_unreachable() {
    let fake = Arc::new(Fake::default());
    fake.token
        .lock()
        .unwrap()
        .push_back((StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "boom" })));
    let url = serve(fake).await;
    let (_dir, store) = store();
    store.set(&url, "r1").unwrap();
    assert_eq!(
        tokens(&url, store.clone()).access_token(None).await,
        TokenAnswer::Unreachable
    );
    assert_eq!(
        tokens("http://127.0.0.1:9", store.clone()).access_token(None).await,
        TokenAnswer::Idle
    );
    store.set("http://127.0.0.1:9", "r").unwrap();
    assert_eq!(
        tokens("http://127.0.0.1:9", store.clone()).access_token(None).await,
        TokenAnswer::Unreachable
    );
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r1"));
}

fn cfg(url: Option<&str>) -> ConfigFile {
    let mut cfg = ConfigFile::default();
    cfg.machine.cloud.url = url.map(String::from);
    cfg
}

#[test]
fn the_cloud_url_is_the_env_then_the_config_then_production() {
    let none = |_: &str| None;
    assert_eq!(cloud_url(&cfg(None), none).unwrap(), "https://retaskable.com");
    assert_eq!(
        cloud_url(&cfg(Some(" https://a.example/ ")), none).unwrap(),
        "https://a.example"
    );
    let env = |k: &str| (k == "RETASKABLE_CLOUD_URL").then(|| "http://localhost:8787/".to_string());
    assert_eq!(
        cloud_url(&cfg(Some("https://a.example")), env).unwrap(),
        "http://localhost:8787"
    );
    assert!(cloud_url(&cfg(Some("ftp://a.example")), none).is_err());
    assert!(cloud_url(&cfg(Some("not a url")), none).is_err());
}

#[test]
fn every_environment_is_a_cloud_and_a_custom_url_is_added_last() {
    let none = |_: &str| None;
    let three = [
        "https://retaskable.com",
        "https://staging.retaskable.com",
        "http://localhost:8787",
    ];
    assert_eq!(cloud_urls(&cfg(None), none), three);
    assert_eq!(cloud_urls(&cfg(Some("https://staging.retaskable.com")), none), three);
    assert_eq!(cloud_urls(&cfg(Some("bad")), none), three);
    assert_eq!(
        cloud_urls(&cfg(Some("https://a.example")), none)[3],
        "https://a.example"
    );
    assert_eq!(environment_name("http://localhost:8787/"), "Local");
    assert_eq!(environment_name("https://a.example"), "Custom");
    assert_eq!(environment_url("staging"), Some("https://staging.retaskable.com"));
    assert_eq!(environment_url("backlog"), None);
}

#[test]
fn the_machine_is_named_by_retaskable_machine_else_the_hostname() {
    let env = |k: &str| (k == "RETASKABLE_MACHINE").then(|| " jcnash-docker ".to_string());
    assert_eq!(machine_name(env), "jcnash-docker");
    assert!(!machine_name(|_| None).is_empty());
    let long = "x".repeat(150);
    let info = machine_info(|k| (k == "RETASKABLE_MACHINE").then(|| long.clone()));
    assert_eq!(info["machine"].as_str().unwrap().len(), 100);
    assert_eq!(info["agentVersion"], env!("CARGO_PKG_VERSION"));
}
