//! The **Link** button of the CI runner's and Chat agent's tabs (`page_host::link`, agent-settings.md section 4.6): the
//! page runs the program's device flow (its role, its own store) and writes the token where the program's own `link`
//! command would, with the same permissions, so the running program picks it up with no restart. Against fake clouds,
//! with the token files named by the programs' own variables so no test touches an OS credential store.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::link::fake_cloud::{self, DEVICE_CODE, FakeCloud, REFRESH, USER_CODE};
use crate::link::{BACKLOG_TARGET, DeviceTokens, TokenAnswer};
use crate::program_link::CI;
use crate::program_status::{self, Linked, Report, StatusWriter};

struct Rig {
    dir: tempfile::TempDir,
    env: Env,
    settings: Arc<SettingsServer>,
    app: Router,
    cookie: String,
    csrf: String,
    cloud: Arc<FakeCloud>,
    store: Arc<FakeCloud>,
    seen: std::sync::Mutex<String>,
}

fn install(root: &Path, program: &'static str, folder: PathBuf, env_set: &[&str]) {
    let now = program_status::now_ms();
    StatusWriter::new(&folder, program, "1.2.3", now)
        .write(
            now,
            Report {
                linked: Linked::default(),
                env_set: env_set.iter().map(|s| s.to_string()).collect(),
                ..Report::default()
            },
        )
        .unwrap();
    let _ = root;
}

impl Rig {
    /// `status_env_set` is what the programs' status files say their own environment sets.
    async fn new(status_env_set: &[&str]) -> Self {
        Self::with(status_env_set, true).await
    }

    /// `page_names_files`: whether this page's own environment names the programs' token files (a container's does).
    async fn with(status_env_set: &[&str], page_names_files: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (cloud, store) = (fake_cloud::spawn().await, fake_cloud::spawn().await);
        let root = dir.path();
        let mut pairs = vec![
            ("RETASKABLE_CLOUD_URL", cloud.base()),
            ("RETASKABLE_BACKLOG_URL", store.base()),
        ];
        if page_names_files {
            pairs.push((
                "RETASKABLE_CI_CREDENTIALS_FILE",
                ci_file(root).to_string_lossy().to_string(),
            ));
            pairs.push((
                "RETASKABLE_CHAT_CREDENTIALS_FILE",
                chat_file(root).to_string_lossy().to_string(),
            ));
        }
        let env = Env::from_pairs(pairs);
        install(
            root,
            program_status::CI_RUNNER,
            crate::paths::ci_dir(root),
            status_env_set,
        );
        install(
            root,
            program_status::CHAT_AGENT,
            crate::paths::chat_dir(root),
            status_env_set,
        );
        let settings = Arc::new(SettingsServer::new(
            root.to_path_buf(),
            4700,
            4700,
            false,
            env.clone(),
            || program_status::now_ms() / 1000,
            |_: i32| {},
            test_keychain(root),
        ));
        settings.set_version("9.9.9");
        settings.program_links.set_second(Duration::from_millis(1));
        let app = app(settings.clone());
        let (cookie, csrf) = sign_in(&app, &settings).await;
        Self {
            dir,
            env,
            settings,
            app,
            cookie,
            csrf,
            cloud,
            store,
            seen: Default::default(),
        }
    }

    async fn record(&self, res: Response) -> (StatusCode, Value) {
        let status = res.status();
        let body = json_body(res).await;
        self.seen.lock().unwrap().push_str(&body.to_string());
        (status, body)
    }

    async fn post(&self, program: &str) -> (StatusCode, Value) {
        let res = self
            .app
            .clone()
            .oneshot(
                request("POST", &format!("/api/settings/programs/{program}/link"))
                    .header("Origin", format!("http://{HOST}"))
                    .header("Cookie", self.cookie.clone())
                    .header("X-CSRF-Token", self.csrf.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        self.record(res).await
    }

    /// The program's row in `GET /api/settings/programs`.
    async fn row(&self, program: &str) -> Value {
        let res = self
            .app
            .clone()
            .oneshot(
                request("GET", "/api/settings/programs")
                    .header("Cookie", self.cookie.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let (_, body) = self.record(res).await;
        body["programs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == program)
            .cloned()
            .unwrap()
    }

    async fn until(&self, program: &str, ok: impl Fn(&Value) -> bool) -> Value {
        let mut changes = self.settings.program_links.subscribe();
        loop {
            let link = self.row(program).await["link"].clone();
            if ok(&link) {
                return link;
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

fn ci_file(root: &Path) -> PathBuf {
    crate::paths::ci_dir(root).join("credentials.json")
}

fn chat_file(root: &Path) -> PathBuf {
    crate::paths::chat_dir(root).join("credentials.json")
}

#[tokio::test]
async fn an_unlinked_program_has_a_link_button_from_this_computer() {
    let rig = Rig::new(&[]).await;
    let ci = rig.row("ci-runner").await;
    assert_eq!(
        (
            ci["link"]["linked"].clone(),
            ci["link"]["here"].clone(),
            ci["link"]["why"].clone()
        ),
        (json!(false), json!(true), Value::Null)
    );
    assert_eq!(
        ci["linkCommand"],
        json!("retaskable-ci link"),
        "the command stays for where the button can't"
    );
    assert_eq!(
        rig.row("chat-agent").await["linkCommand"],
        json!("retaskable-chat-agent link")
    );
}

#[tokio::test]
async fn the_ci_runner_links_as_role_ci_and_its_running_loop_picks_the_token_up() {
    let rig = Rig::new(&[]).await;
    // What the running runner holds: its own store and a token holder made before there was a link.
    let store = CI.store(rig.dir.path(), &rig.env);
    let running = DeviceTokens::new(rig.store.base(), BACKLOG_TARGET, store.clone(), reqwest::Client::new());
    assert_eq!(running.access_token(None).await, TokenAnswer::Idle);

    assert_eq!(
        rig.post("ci-runner").await,
        (StatusCode::OK, json!({ "linking": true }))
    );
    let link = rig.until("ci-runner", |l| l["approve"].is_object()).await;
    assert_eq!(
        link["approve"],
        json!({ "url": format!("{}/link?code={USER_CODE}", rig.store.base()), "code": USER_CODE })
    );
    assert_eq!(
        link["step"],
        json!({ "no": 1, "of": 1, "label": "the shared backlog store" })
    );
    let asked = rig.store.codes.lock().unwrap()[0].clone();
    assert_eq!(
        (asked["role"].clone(), asked["agentVersion"].clone()),
        (json!("ci"), json!("9.9.9"))
    );
    assert_eq!(
        rig.cloud.asked(),
        0,
        "the CI runner never links to retaskable.com's Worker"
    );

    rig.store.approve();
    let link = rig.until("ci-runner", |l| l["linked"] == json!(true)).await;
    assert_eq!(
        (link["working"].clone(), link["error"].clone()),
        (json!(false), Value::Null)
    );

    // The token is in the runner's own file, under its own service, readable by this user only.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(ci_file(rig.dir.path())).unwrap()).unwrap();
    assert_eq!(saved["retaskable-ci"][rig.store.base()], json!(REFRESH));
    assert!(saved.get("retaskable").is_none() && saved.get("retaskable-chat").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(ci_file(rig.dir.path())).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The loop made before the link finds it on its next token, with no restart.
    assert!(matches!(running.access_token(None).await, TokenAnswer::Token { .. }));
    // Nothing of the Coding Agent's own link was touched.
    assert_eq!(rig.settings.keychain.get(&rig.store.base()).unwrap(), None);
    rig.nothing_secret_was_sent();
}

#[tokio::test]
async fn the_chat_agent_links_retaskable_com_then_the_store_as_role_chat() {
    let rig = Rig::new(&[]).await;
    assert_eq!(rig.post("chat-agent").await.0, StatusCode::OK);
    let link = rig.until("chat-agent", |l| l["approve"].is_object()).await;
    assert_eq!(link["step"], json!({ "no": 1, "of": 2, "label": "retaskable.com" }));
    assert_eq!(
        rig.store.asked(),
        0,
        "the store's code is asked for only once the first is approved"
    );
    rig.cloud.approve();
    let link = rig
        .until("chat-agent", |l| {
            l["step"]["no"] == json!(2) && l["approve"].is_object()
        })
        .await;
    assert_eq!(link["step"]["label"], json!("the shared backlog store"));
    rig.store.approve();
    rig.until("chat-agent", |l| l["linked"] == json!(true)).await;

    for fake in [&rig.cloud, &rig.store] {
        assert_eq!(fake.codes.lock().unwrap()[0]["role"], json!("chat"));
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(chat_file(rig.dir.path())).unwrap()).unwrap();
    assert_eq!(saved["retaskable-chat"][rig.cloud.base()], json!(REFRESH));
    assert_eq!(saved["retaskable-chat"][rig.store.base()], json!(REFRESH));
    rig.nothing_secret_was_sent();
}

#[tokio::test]
async fn a_denied_link_says_so_and_can_be_tried_again() {
    let rig = Rig::new(&[]).await;
    rig.post("ci-runner").await;
    rig.store.refuse("access_denied");
    let link = rig.until("ci-runner", |l| l["error"].is_string()).await;
    assert!(link["error"].as_str().unwrap().contains("denied"), "{link}");
    assert_eq!(link["linked"], json!(false));
    assert!(!ci_file(rig.dir.path()).exists());

    rig.store.refuse("expired_token");
    assert_eq!(rig.post("ci-runner").await.0, StatusCode::OK);
    let link = rig
        .until("ci-runner", |l| {
            l["error"].as_str().is_some_and(|e| e.contains("expired"))
        })
        .await;
    assert_eq!(link["working"], json!(false));
    rig.nothing_secret_was_sent();
}

#[tokio::test]
async fn only_one_flow_at_a_time_and_none_for_a_program_already_linked() {
    let rig = Rig::new(&[]).await;
    assert_eq!(rig.post("ci-runner").await.0, StatusCode::OK);
    rig.until("ci-runner", |l| l["approve"].is_object()).await;
    let (status, body) = rig.post("ci-runner").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already waiting"));
    assert_eq!(rig.store.asked(), 1);
    // The chat agent's is its own flow.
    assert_eq!(rig.post("chat-agent").await.0, StatusCode::OK);

    rig.store.approve();
    rig.until("ci-runner", |l| l["linked"] == json!(true)).await;
    let (status, body) = rig.post("ci-runner").await;
    assert_eq!(
        (status, body["error"].clone()),
        (StatusCode::CONFLICT, json!("Already linked."))
    );
}

#[tokio::test]
async fn a_program_the_page_cannot_link_is_refused_and_says_to_use_the_command() {
    // The runner's token file is named in its own environment, which this page's isn't: the page can't see where.
    let rig = Rig::with(&["RETASKABLE_CI_CREDENTIALS_FILE"], false).await;
    let link = rig.row("ci-runner").await["link"].clone();
    assert!(
        link["why"].as_str().unwrap().contains("RETASKABLE_CI_CREDENTIALS_FILE"),
        "{link}"
    );
    let (status, body) = rig.post("ci-runner").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("terminal"), "{body}");
    // The Chat agent's own environment names no file, so the page links it.
    assert_eq!(rig.row("chat-agent").await["link"]["why"], Value::Null);
    // The same program on a page whose environment has the file links.
    let rig = Rig::new(&["RETASKABLE_CI_CREDENTIALS_FILE"]).await;
    assert_eq!(rig.row("ci-runner").await["link"]["why"], Value::Null);
    assert_eq!(rig.store.asked(), 0);
}

#[test]
fn a_bad_store_address_is_said_before_anything_is_sent() {
    let bad = Env::from_pairs([("RETASKABLE_BACKLOG_URL", "not a url")]);
    let err = CI.steps(&bad).err().unwrap().to_string();
    assert!(err.contains("backlog store"), "{err}");
}

#[tokio::test]
async fn a_program_not_installed_or_not_linkable_is_refused() {
    let rig = Rig::new(&[]).await;
    assert_eq!(rig.post("code-agent").await.0, StatusCode::NOT_FOUND);
    assert_eq!(rig.post("nothing").await.0, StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(crate::paths::chat_dir(rig.dir.path())).unwrap();
    let (status, body) = rig.post("chat-agent").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("isn't installed"));
    assert_eq!(rig.cloud.asked() + rig.store.asked(), 0);
}

#[tokio::test]
async fn a_program_link_needs_a_session_the_csrf_token_and_this_computer() {
    let rig = Rig::new(&[]).await;
    let send = |cookie: Option<&str>, csrf: Option<&str>| {
        let mut req =
            request("POST", "/api/settings/programs/ci-runner/link").header("Origin", format!("http://{HOST}"));
        if let Some(c) = cookie {
            req = req.header("Cookie", c.to_string());
        }
        if let Some(c) = csrf {
            req = req.header("X-CSRF-Token", c.to_string());
        }
        rig.app.clone().oneshot(req.body(Body::empty()).unwrap())
    };
    assert_eq!(send(None, None).await.unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(Some(&rig.cookie), None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(Some(&rig.cookie), Some("nope")).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let get = request("GET", "/api/settings/programs/ci-runner/link")
        .header("Cookie", rig.cookie.clone())
        .body(Body::empty())
        .unwrap();
    assert!(!rig.app.clone().oneshot(get).await.unwrap().status().is_success());
    assert_eq!(rig.store.asked(), 0);
}
