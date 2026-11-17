//! **Add project** over HTTP (`project_routes.rs`, agent-settings.md section 4.7): the picker's lists from a fake `gh`,
//! an add with a fake clone (a temp repo) through to `config.json`, the paused start and the restart, and what is refused
//! (no session, no CSRF, another computer, a bad or repeated name, a failed clone). No token reaches a response.
use std::sync::atomic::{AtomicI32, Ordering};

use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::add_project::AddProject;
use crate::add_project::tests::{LEAKY, make_repo};
use crate::repo::RepoSummary;
use crate::repo_picker::RepoPicker;

struct Rig {
    dir: tempfile::TempDir,
    app: Router,
    cookie: String,
    csrf: String,
    /// The code agent's exit code once an add asked it to restart, else 0.
    exited: Arc<AtomicI32>,
    /// The agent's own view the page's Pause and Resume change (it shares the pause with the connectors and the driver).
    view: crate::view::AgentView,
    seen: Mutex<String>,
}

impl Rig {
    async fn new(clone_fails: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let first = dir.path().join("code").join("first");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::write(
            data.join("config.json"),
            json!({ "version": 2, "projects": [{ "id": "first", "repoPath": first, "githubRepo": "me/first", "baseBranch": "main" }] })
                .to_string(),
        )
        .unwrap();
        let exited = Arc::new(AtomicI32::new(0));
        let code = exited.clone();
        let server = Arc::new(SettingsServer::new(
            data.clone(),
            4700,
            4700,
            false,
            Env::from_pairs([("GH_TOKEN", LEAKY)]),
            || 1_700_000_000,
            |_: i32| {},
            test_keychain(&data),
        ));
        let repo = |name: &str| RepoSummary {
            name_with_owner: name.into(),
            description: Some("A thing".into()),
            visibility: "PRIVATE".into(),
            is_fork: false,
        };
        let add = AddProject::with(
            data.clone(),
            Env::from_pairs([("GH_TOKEN", LEAKY)]),
            RepoPicker::with(
                Box::new(|| Ok(vec!["me".into(), "Org".into()])),
                Box::new(move |owner| {
                    if owner == "Org" {
                        Ok(vec![repo("Org/app")])
                    } else {
                        Ok(vec![repo("me/first"), repo("me/other")])
                    }
                }),
                Box::new(std::time::Instant::now),
            ),
            Box::new(move |name, dest| {
                if clone_fails {
                    anyhow::bail!("Couldn't clone {name}: not found (token {LEAKY})");
                }
                make_repo(dest, None);
                Ok(())
            }),
            Box::new(|_| Some("trunk".into())),
            // Inline, so the answer to the POST is the finished add: no waiting on a thread.
            Box::new(|job| job()),
            Box::new(move || code.store(75, Ordering::SeqCst)),
        );
        server.set_add_project(add);
        let view =
            crate::test_support::agent_view(vec![], vec![crate::test_support::project_view("first")], "1.2.3", 1);
        let view = crate::view::AgentView {
            paused: crate::view::PauseState::persisted(&data.join("paused.json")),
            ..view
        };
        server.set_status_source(crate::settings_server::StatusSource {
            view: view.clone(),
            started: std::time::Instant::now(),
            checks: Default::default(),
            live: Default::default(),
        });
        let app = app(server.clone());
        let (cookie, csrf) = sign_in(&app, &server).await;
        Self {
            dir,
            app,
            cookie,
            csrf,
            exited,
            view,
            seen: Default::default(),
        }
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = json_body(res).await;
        self.seen.lock().unwrap().push_str(&body.to_string());
        (status, body)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let req = request("GET", path)
            .header("Cookie", self.cookie.clone())
            .body(Body::empty())
            .unwrap();
        self.send(req).await
    }

    async fn add(&self, repo: &str) -> (StatusCode, Value) {
        let req = request("POST", "/api/projects")
            .header("Origin", format!("http://{HOST}"))
            .header("Cookie", self.cookie.clone())
            .header("X-CSRF-Token", self.csrf.clone())
            .body(Body::from(json!({ "repo": repo }).to_string()))
            .unwrap();
        self.send(req).await
    }

    fn config(&self) -> Value {
        let text = std::fs::read_to_string(self.dir.path().join("data/config.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn no_token_was_sent(&self) {
        let seen = self.seen.lock().unwrap();
        assert!(
            !seen.contains(LEAKY) && !seen.contains("ghp_"),
            "a token reached the page: {seen}"
        );
    }
}

#[tokio::test]
async fn the_picker_lists_owners_then_an_owners_repos_marking_the_ones_added() {
    let rig = Rig::new(false).await;
    let (status, body) = rig.get("/api/projects/owners").await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "owners": ["me", "Org"] })));
    let (status, body) = rig.get("/api/projects/repos?owner=me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["repos"],
        json!([
            { "nameWithOwner": "me/first", "description": "A thing", "private": true, "fork": false, "added": true },
            { "nameWithOwner": "me/other", "description": "A thing", "private": true, "fork": false, "added": false },
        ])
    );
    let (status, _) = rig.get("/api/projects/repos?owner=stranger").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = rig.get("/api/projects/repos").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no owner");
    rig.no_token_was_sent();
}

#[tokio::test]
async fn an_add_clones_writes_the_entry_starts_it_paused_and_restarts_the_agent() {
    let rig = Rig::new(false).await;
    let (status, body) = rig.add("Org/app").await;
    assert_eq!((status, body), (StatusCode::ACCEPTED, json!({ "adding": true })));
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["canAdd"], true);
    assert_eq!(view["add"]["state"], "done");
    assert_eq!(view["add"]["id"], "app");
    assert_eq!(view["add"]["paused"], true);
    assert_eq!(view["projects"][1]["id"], "app");
    assert_eq!(view["projects"][1]["githubRepo"], "Org/app");
    let entry = &rig.config()["projects"][1];
    assert_eq!(entry["baseBranch"], "trunk");
    assert_eq!(entry["backlogPath"], "docs/specs/engines/10-build-order.md");
    assert!(
        rig.dir.path().join("code/app/.git").exists(),
        "cloned beside the first project"
    );
    // Held to start paused, and restarted to pick it up.
    let held = std::fs::read_to_string(rig.dir.path().join("data/start-paused.json")).unwrap();
    assert!(held.contains("\"hold\"") && held.contains("app"), "{held}");
    assert_eq!(rig.exited.load(Ordering::SeqCst), 75);
    rig.no_token_was_sent();
}

#[tokio::test]
async fn a_repo_added_already_and_a_bad_name_are_refused_in_words() {
    let rig = Rig::new(false).await;
    let (status, body) = rig.add("me/FIRST").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("added already"), "{body}");
    for bad in ["nope", "a/b/c", "../x", ""] {
        let (status, body) = rig.add(bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert!(body["error"].as_str().unwrap().contains("owner/name"), "{body}");
    }
    let (status, _) = rig
        .send(
            request("POST", "/api/projects")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", rig.cookie.clone())
                .header("X-CSRF-Token", rig.csrf.clone())
                .body(Body::from("not json"))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(rig.config()["projects"].as_array().unwrap().len(), 1);
    assert_eq!(rig.exited.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_failed_clone_is_shown_in_words_with_no_token_and_changes_nothing() {
    let rig = Rig::new(true).await;
    let before = rig.config();
    assert_eq!(rig.add("Org/app").await.0, StatusCode::ACCEPTED);
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["add"]["state"], "failed");
    let message = view["add"]["message"].as_str().unwrap();
    assert!(message.contains("Couldn't clone Org/app"), "{message}");
    assert_eq!(rig.config(), before);
    assert_eq!(rig.exited.load(Ordering::SeqCst), 0);
    rig.no_token_was_sent();
}

#[tokio::test]
async fn an_add_needs_a_session_and_the_csrf_token_and_a_get_is_no_way_to_add() {
    let rig = Rig::new(false).await;
    let post = |cookie: Option<&str>, csrf: Option<&str>| {
        let mut req = request("POST", "/api/projects").header("Origin", format!("http://{HOST}"));
        if let Some(c) = cookie {
            req = req.header("Cookie", c.to_string());
        }
        if let Some(c) = csrf {
            req = req.header("X-CSRF-Token", c.to_string());
        }
        rig.app
            .clone()
            .oneshot(req.body(Body::from(json!({ "repo": "Org/app" }).to_string())).unwrap())
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
    let foreign = request("POST", "/api/projects")
        .header("Origin", "http://evil.example")
        .header("Cookie", rig.cookie.clone())
        .header("X-CSRF-Token", rig.csrf.clone())
        .body(Body::from(json!({ "repo": "Org/app" }).to_string()))
        .unwrap();
    assert!(!rig.app.clone().oneshot(foreign).await.unwrap().status().is_success());
    let unsigned = request("GET", "/api/projects").body(Body::empty()).unwrap();
    assert_eq!(
        rig.app.clone().oneshot(unsigned).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        rig.config()["projects"].as_array().unwrap().len(),
        1,
        "none of them added it"
    );
    assert!(!rig.dir.path().join("code/app").exists());
}

#[tokio::test]
async fn another_computer_signed_in_by_password_can_look_but_cannot_add() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        true,
        Env::empty(),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    );
    let own: std::net::IpAddr = "192.168.1.20".parse().unwrap();
    set_own_addresses(&mut server, vec![own]);
    MachineStore::load(dir.path())
        .unwrap()
        .set_network(NetworkAccess {
            enabled: true,
            password_hash: Some(crate::password::hash_password("a good enough password")),
        })
        .unwrap();
    server.set_add_project(AddProject::with(
        dir.path().to_path_buf(),
        Env::empty(),
        RepoPicker::new(),
        Box::new(|_, _| panic!("a clone started for another computer")),
        Box::new(|_| None),
        Box::new(|job| job()),
        Box::new(|| panic!("a restart for another computer")),
    ));
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
                .uri("/api/projects")
                .header("Host", host.clone())
                .header("Cookie", cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(json_body(view).await["canAdd"], json!(false));
    let res = remote
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header("Host", host.clone())
                .header("Origin", format!("http://{host}"))
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::from(json!({ "repo": "Org/app" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(!dir.path().join("config.json").exists());
}

#[test]
fn the_page_has_the_add_project_card_and_its_script_builds_no_markup_and_holds_no_token() {
    let page = include_str!("../../assets/settings.html");
    assert_eq!(page.matches("id=\"projectsBox\"").count(), 1);
    assert_eq!(page.matches("{{PROJECTS_JS}}").count(), 1);
    let js = include_str!("../../assets/settings-projects.js");
    assert!(!js.contains("innerHTML") && !js.contains("insertAdjacentHTML") && !js.contains("fetch("));
    assert!(js.contains("\"Add project\"") && js.contains("\"Resume\"") && js.contains("\"Pause\""));
    assert!(js.lines().count() < 1000 && page.lines().count() < 1000);
    // The served page has the script in it, and no placeholder left.
    let served = crate::page_host::assemble(page);
    assert!(served.contains("RT.projects = {") && !served.contains("{{PROJECTS_JS}}"));
}

#[test]
fn each_project_is_a_row_with_a_state_chip_and_the_profile_is_read_again_after_every_settings_load() {
    let js = include_str!("../../assets/settings-projects.js");
    assert!(js.contains("\"proj\"") && js.contains("\"Working\"") && js.contains("\"Paused for every computer\""));
    // A GH_TOKEN saved on the page must show in "Who this computer works as" without a restart: the profile is
    // fetched by loadSettings (run after every save), not only once at sign-in.
    let page = include_str!("../../assets/settings.html");
    let load = page
        .split("function loadSettings()")
        .nth(1)
        .unwrap()
        .split("\n  }")
        .next()
        .unwrap();
    assert!(load.contains("loadProfile()"));
    assert_eq!(page.matches("api(\"/api/profile\")").count(), 1);
}

impl Rig {
    async fn pause(&self, id: &str, paused: bool) -> (StatusCode, Value) {
        let req = request("POST", &format!("/api/projects/{id}/paused"))
            .header("Origin", format!("http://{HOST}"))
            .header("Cookie", self.cookie.clone())
            .header("X-CSRF-Token", self.csrf.clone())
            .body(Body::from(json!({ "paused": paused }).to_string()))
            .unwrap();
        self.send(req).await
    }

    fn paused_file(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.dir.path().join("data/paused.json")).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["projects"]["first"]
            .as_str()
            .map(str::to_string)
    }
}

#[tokio::test]
async fn pause_and_resume_set_this_computers_pause_live_and_keep_it_across_a_restart() {
    let rig = Rig::new(false).await;
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["canPause"], true);
    assert_eq!(view["projects"][0]["paused"], Value::Null);
    let mut snapshots = rig.view.paused.subscribe();
    snapshots.mark_unchanged();

    let (status, body) = rig.pause("first", true).await;
    assert_eq!(
        (status, body["paused"].as_str()),
        (StatusCode::OK, Some("Paused from the settings page"))
    );
    // Live: the pass reads this state, and the hub's snapshot is resent by the change.
    assert!(rig.view.paused.reason("first").is_some());
    assert!(
        snapshots.has_changed().unwrap(),
        "the change resends the project's snapshot"
    );
    assert_eq!(rig.paused_file().as_deref(), Some("Paused from the settings page"));
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["projects"][0]["paused"], "Paused from the settings page");
    // A restarted agent reads it back before its first pass, so it sends no run_take.
    let reread = crate::view::PauseState::persisted(&rig.dir.path().join("data/paused.json"));
    assert!(reread.reason("first").is_some());

    let (status, body) = rig.pause("first", false).await;
    assert_eq!((status, body["paused"].clone()), (StatusCode::OK, Value::Null));
    assert!(rig.view.paused.reason("first").is_none());
    assert_eq!(rig.paused_file(), None);
    // Pausing twice, or resuming what isn't paused, is no error.
    assert_eq!(rig.pause("first", false).await.0, StatusCode::OK);
    rig.no_token_was_sent();
}

#[tokio::test]
async fn the_pages_pause_is_the_hubs_computer_pause_with_the_pages_own_reason() {
    let rig = Rig::new(false).await;
    crate::commands::pause_computer(&rig.view, "first", "the settings page");
    let via_hub_path = rig.view.paused.reason("first");
    crate::commands::resume_computer(&rig.view, "first");
    rig.pause("first", true).await;
    assert_eq!(rig.view.paused.reason("first"), via_hub_path);
    assert!(
        via_hub_path.unwrap().starts_with("Paused from "),
        "the site's and the chat's words"
    );
}

#[tokio::test]
async fn a_pause_the_agent_started_with_is_shown_with_its_reason_and_resumed_from_the_page() {
    let rig = Rig::new(false).await;
    let why = "Paused at its first start (RETASKABLE_START_PAUSED): resume it when it's ready";
    rig.view.paused.set("first", Some(why.into()));
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["projects"][0]["paused"], why);
    assert_eq!(rig.pause("first", false).await.0, StatusCode::OK);
    assert!(rig.view.paused.reason("first").is_none());
}

#[tokio::test]
async fn the_stores_own_pause_is_shown_read_only_and_resuming_this_computer_leaves_it() {
    let rig = Rig::new(false).await;
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["projects"][0]["storePaused"], Value::Null, "not known yet");
    rig.view
        .store_paused
        .set("first", crate::view::StorePause::Paused("Paused by Sam".into()));
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["projects"][0]["storePaused"], true);
    assert_eq!(view["projects"][0]["storePausedReason"], "Paused by Sam");
    rig.pause("first", true).await;
    rig.pause("first", false).await;
    assert_eq!(
        rig.view.store_paused.get("first"),
        crate::view::StorePause::Paused("Paused by Sam".into())
    );
    rig.view.store_paused.set("first", crate::view::StorePause::NotPaused);
    let (_, view) = rig.get("/api/projects").await;
    assert_eq!(view["projects"][0]["storePaused"], false);
}

#[tokio::test]
async fn pausing_an_unknown_project_is_404_and_a_bad_body_is_400() {
    let rig = Rig::new(false).await;
    assert_eq!(rig.pause("nope", true).await.0, StatusCode::NOT_FOUND);
    assert!(rig.view.paused.reason("nope").is_none());
    let req = request("POST", "/api/projects/first/paused")
        .header("Origin", format!("http://{HOST}"))
        .header("Cookie", rig.cookie.clone())
        .header("X-CSRF-Token", rig.csrf.clone())
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(rig.send(req).await.0, StatusCode::BAD_REQUEST);
    assert!(rig.view.paused.reason("first").is_none());
}

#[tokio::test]
async fn pause_needs_a_session_the_csrf_token_and_this_computer() {
    let rig = Rig::new(false).await;
    let post = |cookie: Option<&str>, csrf: Option<&str>| {
        let mut req = request("POST", "/api/projects/first/paused").header("Origin", format!("http://{HOST}"));
        if let Some(c) = cookie {
            req = req.header("Cookie", c.to_string());
        }
        if let Some(c) = csrf {
            req = req.header("X-CSRF-Token", c.to_string());
        }
        rig.app
            .clone()
            .oneshot(req.body(Body::from(json!({ "paused": true }).to_string())).unwrap())
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
    let foreign = request("POST", "/api/projects/first/paused")
        .header("Origin", "http://evil.example")
        .header("Cookie", rig.cookie.clone())
        .header("X-CSRF-Token", rig.csrf.clone())
        .body(Body::from(json!({ "paused": true }).to_string()))
        .unwrap();
    assert!(!rig.app.clone().oneshot(foreign).await.unwrap().status().is_success());
    assert!(rig.view.paused.reason("first").is_none(), "none of them paused it");
}

#[tokio::test]
async fn another_computer_signed_in_by_password_cannot_pause_or_resume() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        true,
        Env::empty(),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    );
    let own: std::net::IpAddr = "192.168.1.20".parse().unwrap();
    set_own_addresses(&mut server, vec![own]);
    MachineStore::load(dir.path())
        .unwrap()
        .set_network(NetworkAccess {
            enabled: true,
            password_hash: Some(crate::password::hash_password("a good enough password")),
        })
        .unwrap();
    server.set_add_project(AddProject::with(
        dir.path().to_path_buf(),
        Env::empty(),
        RepoPicker::new(),
        Box::new(|_, _| panic!("a clone")),
        Box::new(|_| None),
        Box::new(|job| job()),
        Box::new(|| panic!("a restart")),
    ));
    let view = crate::test_support::agent_view(vec![], vec![crate::test_support::project_view("first")], "1", 1);
    server.set_status_source(crate::settings_server::StatusSource {
        view: view.clone(),
        started: std::time::Instant::now(),
        checks: Default::default(),
        live: Default::default(),
    });
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
    let cookie = res.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let csrf = json_body(res).await["csrfToken"].as_str().unwrap().to_string();
    let res = remote
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects/first/paused")
                .header("Host", host.clone())
                .header("Origin", format!("http://{host}"))
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::from(json!({ "paused": true }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(view.paused.reason("first").is_none());
}
