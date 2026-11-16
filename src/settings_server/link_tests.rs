//! The code agent's **Link** buttons (`link_routes.rs`, agent-settings.md section 4.6): start, wait, approve, denied and
//! expired against a fake cloud and a fake store, one flow at a time, and what is refused (no session, no CSRF, another
//! computer, a link already held). The fake's device code and refresh token never appear in anything the page is sent.
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::link::fake_cloud::{self, DEVICE_CODE, FakeCloud, REFRESH, USER_CODE};
use crate::link::pending::{Links, Opener};

/// A signed-in page whose Custom cloud is `cloud` and whose shared backlog is `store`, and a browser that is never to be opened.
struct Rig {
    _dir: tempfile::TempDir,
    links: Links,
    keychain: Arc<dyn TokenStore>,
    app: Router,
    cookie: String,
    csrf: String,
    cloud: Arc<FakeCloud>,
    store: Arc<FakeCloud>,
    /// Every body the page answered, to look for a secret in.
    seen: std::sync::Mutex<String>,
}

impl Rig {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (cloud, store) = (fake_cloud::spawn().await, fake_cloud::spawn().await);
        let env = Env::from_pairs([
            ("RETASKABLE_CLOUD_URL", cloud.base()),
            ("RETASKABLE_BACKLOG_URL", store.base()),
        ]);
        let keychain = test_keychain(dir.path());
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            4700,
            false,
            env,
            || 1_700_000_000,
            |_: i32| {},
            keychain.clone(),
        ));
        let opener: Opener = Arc::new(|url| panic!("the page opens no browser, but {url} was"));
        let links = Links::with(
            keychain.clone(),
            reqwest::Client::new(),
            json!({ "machine": "pc", "os": "Linux", "agentVersion": "1.2.3" }),
            opener,
            Duration::from_millis(1),
        );
        settings.set_links(links.clone());
        let app = app(settings.clone());
        let (cookie, csrf) = sign_in(&app, &settings).await;
        Self {
            _dir: dir,
            links,
            keychain,
            app,
            cookie,
            csrf,
            cloud,
            store,
            seen: Default::default(),
        }
    }

    async fn answer(&self, res: Response) -> (StatusCode, Value) {
        let status = res.status();
        let body = json_body(res).await;
        self.seen.lock().unwrap().push_str(&body.to_string());
        (status, body)
    }

    async fn post(&self, target: &str) -> (StatusCode, Value) {
        let res = self
            .app
            .clone()
            .oneshot(
                request("POST", &format!("/api/link/{target}"))
                    .header("Origin", format!("http://{HOST}"))
                    .header("Cookie", self.cookie.clone())
                    .header("X-CSRF-Token", self.csrf.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        self.answer(res).await
    }

    async fn view(&self) -> Value {
        let res = self
            .app
            .clone()
            .oneshot(
                request("GET", "/api/link")
                    .header("Cookie", self.cookie.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        self.answer(res).await.1
    }

    fn row(view: &Value, name: &str) -> Value {
        let all = view["environments"].as_array().unwrap();
        all.iter()
            .find(|e| e["name"] == name)
            .cloned()
            .unwrap_or_else(|| view["backlog"].clone())
    }

    /// Waits for `ok` of the page's view, waking on every change of the links' state.
    async fn until(&self, ok: impl Fn(&Value) -> bool) -> Value {
        let mut changes = self.links.subscribe();
        loop {
            let view = self.view().await;
            if ok(&view) {
                return view;
            }
            tokio::time::timeout(Duration::from_secs(10), changes.changed())
                .await
                .expect("the link's state never changed")
                .unwrap();
        }
    }

    fn nothing_secret_was_sent(&self) {
        let seen = self.seen.lock().unwrap();
        for secret in [DEVICE_CODE, REFRESH] {
            assert!(!seen.contains(secret), "{secret} reached the page: {seen}");
        }
    }
}

#[tokio::test]
async fn the_page_shows_each_target_not_linked_and_that_this_computer_can_link_it() {
    let rig = Rig::new().await;
    let view = rig.view().await;
    let names: Vec<_> = view["environments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].clone())
        .collect();
    assert_eq!(names, ["Production", "Staging", "Local", "Custom"]);
    assert_eq!(view["canLink"], json!(true));
    assert_eq!(view["backlog"]["connected"], json!(false));
    assert!(
        view["environments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["connected"] == json!(false))
    );
}

#[tokio::test]
async fn a_link_shows_its_address_and_code_then_links_and_goes_on_to_the_shared_backlog() {
    let rig = Rig::new().await;
    let (status, body) = rig.post("custom").await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "linking": true })));

    let view = rig.until(|v| Rig::row(v, "Custom")["waiting"].is_object()).await;
    let waiting = Rig::row(&view, "Custom")["waiting"].clone();
    assert_eq!(
        waiting,
        json!({ "url": format!("{}/link?code={USER_CODE}", rig.cloud.base()), "code": USER_CODE })
    );
    assert_eq!(Rig::row(&view, "Custom")["connected"], json!(false));
    assert_eq!(rig.cloud.codes.lock().unwrap()[0]["agentVersion"], json!("1.2.3"));

    // Approving retaskable.com's code links it, and the store's own code follows, as `retaskable link` does.
    rig.cloud.approve();
    let view = rig
        .until(|v| Rig::row(v, "Custom")["connected"] == json!(true) && v["backlog"]["waiting"].is_object())
        .await;
    assert_eq!(view["backlog"]["waiting"]["code"], json!(USER_CODE));
    assert_eq!(rig.store.asked(), 1);
    assert_eq!(
        rig.keychain.get(&rig.cloud.base()).unwrap().as_deref(),
        Some(REFRESH),
        "the token is in this agent's own store"
    );
    // The tray's "Waiting for you" shows the same approval.
    assert!(rig.links.backlog().is_some());

    rig.store.approve();
    let view = rig.until(|v| v["backlog"]["connected"] == json!(true)).await;
    assert_eq!(view["backlog"]["waiting"], Value::Null);
    assert!(rig.links.backlog().is_none());
    rig.nothing_secret_was_sent();
}

#[tokio::test]
async fn a_denied_code_says_so_and_the_button_can_be_pressed_again() {
    let rig = Rig::new().await;
    rig.post("custom").await;
    rig.cloud.refuse("access_denied");
    let view = rig.until(|v| Rig::row(v, "Custom")["error"].is_string()).await;
    let row = Rig::row(&view, "Custom");
    assert!(row["error"].as_str().unwrap().contains("denied"), "{row}");
    assert_eq!(
        (row["connected"].clone(), row["working"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(rig.store.asked(), 0, "no backlog link follows a link that failed");

    rig.cloud.approve();
    assert_eq!(rig.post("custom").await.0, StatusCode::OK);
    let view = rig.until(|v| Rig::row(v, "Custom")["connected"] == json!(true)).await;
    assert_eq!(Rig::row(&view, "Custom")["error"], Value::Null);
}

#[tokio::test]
async fn an_expired_code_says_so() {
    let rig = Rig::new().await;
    rig.post("custom").await;
    rig.cloud.refuse("expired_token");
    let view = rig.until(|v| Rig::row(v, "Custom")["error"].is_string()).await;
    assert!(Rig::row(&view, "Custom")["error"].as_str().unwrap().contains("expired"));
    rig.nothing_secret_was_sent();
}

#[tokio::test]
async fn the_shared_backlog_has_its_own_button() {
    let rig = Rig::new().await;
    assert_eq!(rig.post("backlog").await.0, StatusCode::OK);
    rig.until(|v| v["backlog"]["waiting"].is_object()).await;
    assert!(rig.links.backlog().is_some(), "the tray shows it too");
    rig.store.approve();
    rig.until(|v| v["backlog"]["connected"] == json!(true)).await;
    assert_eq!(rig.cloud.asked(), 0);
}

#[tokio::test]
async fn only_one_flow_at_a_time_per_target() {
    let rig = Rig::new().await;
    assert_eq!(rig.post("custom").await.0, StatusCode::OK);
    rig.until(|v| Rig::row(v, "Custom")["waiting"].is_object()).await;
    let (status, body) = rig.post("custom").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already waiting"));
    assert_eq!(rig.cloud.asked(), 1, "the second press asked the cloud for nothing");
    // Another target is its own flow.
    assert_eq!(rig.post("backlog").await.0, StatusCode::OK);
    rig.until(|v| v["backlog"]["waiting"].is_object()).await;
    assert_eq!(rig.store.asked(), 1);
}

#[tokio::test]
async fn a_target_already_linked_or_not_there_is_refused() {
    let rig = Rig::new().await;
    rig.keychain.set(&rig.cloud.base(), "held").unwrap();
    let (status, body) = rig.post("custom").await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::CONFLICT, json!("Already linked."))
    );
    assert_eq!(rig.post("elsewhere").await.0, StatusCode::NOT_FOUND);
    assert_eq!(rig.cloud.asked() + rig.store.asked(), 0);
}

#[tokio::test]
async fn a_link_needs_a_session_and_the_csrf_token() {
    let rig = Rig::new().await;
    let post = |cookie: Option<&str>, csrf: Option<&str>| {
        let mut req = request("POST", "/api/link/custom").header("Origin", format!("http://{HOST}"));
        if let Some(c) = cookie {
            req = req.header("Cookie", c.to_string());
        }
        if let Some(c) = csrf {
            req = req.header("X-CSRF-Token", c.to_string());
        }
        rig.app.clone().oneshot(req.body(Body::empty()).unwrap())
    };
    assert_eq!(post(None, None).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        post(Some(&rig.cookie), None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(Some(&rig.cookie), Some("wrong")).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    // Not from another page: the Origin must be this one's own.
    let foreign = request("POST", "/api/link/custom")
        .header("Origin", "http://evil.example")
        .header("Cookie", rig.cookie.clone())
        .header("X-CSRF-Token", rig.csrf.clone())
        .body(Body::empty())
        .unwrap();
    assert!(!rig.app.clone().oneshot(foreign).await.unwrap().status().is_success());
    assert_eq!(rig.cloud.asked(), 0, "none of them started a link");
    // And a GET is no way to start one.
    let get = request("GET", "/api/link/custom")
        .header("Cookie", rig.cookie.clone())
        .body(Body::empty())
        .unwrap();
    assert!(!rig.app.clone().oneshot(get).await.unwrap().status().is_success());
}

#[tokio::test]
async fn another_computer_signed_in_by_password_cannot_link() {
    let dir = tempfile::tempdir().unwrap();
    let cloud = fake_cloud::spawn().await;
    let mut server = SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        true,
        Env::from_pairs([("RETASKABLE_CLOUD_URL", cloud.base())]),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    );
    server.set_links(Links::new(
        test_keychain(dir.path()),
        reqwest::Client::new(),
        &Env::empty(),
        "1.2.3",
    ));
    let own: std::net::IpAddr = "192.168.1.20".parse().unwrap();
    set_own_addresses(&mut server, vec![own]);
    MachineStore::load(dir.path())
        .unwrap()
        .set_network(NetworkAccess {
            enabled: true,
            password_hash: Some(crate::password::hash_password("a good enough password")),
        })
        .unwrap();
    let remote = app_from(Arc::new(server), "192.168.1.50".parse().unwrap());
    let host = format!("{own}:4700");
    let res = remote
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/settings/session")
                .header("Host", host.clone())
                .header("Origin", format!("http://{host}"))
                .body(Body::from(json!({ "password": "a good enough password" }).to_string()))
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
    let csrf = json_body(res).await["csrfToken"].as_str().unwrap().to_string();
    let view = remote
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/link")
                .header("Host", host.clone())
                .header("Cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(view).await["canLink"], json!(false));
    let res = remote
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/link/custom")
                .header("Host", host.clone())
                .header("Origin", format!("http://{host}"))
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(cloud.asked(), 0);
}
