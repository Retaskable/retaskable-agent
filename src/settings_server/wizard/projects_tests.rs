//! The Projects step's retaskable.com list: its grouping, the "on this computer" mark, the fallback to GitHub when the
//! cloud has no such route, and that a ticked project is linked into its organisation. The ask itself is made through the
//! agent's one cloud link (a made-up `CloudToken` here, a local server for the cloud).
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode as Status};
use axum::routing::get;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::projects::*;
use crate::add_project::AddProject;
use crate::add_project::tests::{LEAKY, make_repo};
use crate::env::Env;
use crate::link::TokenAnswer;
use crate::repo::RepoSummary;
use crate::repo_picker::RepoPicker;
use crate::settings_server::test_support::*;
use crate::settings_server::{SettingsServer, test_keychain};
use crate::view::CloudToken;

fn row(org: (&str, &str), project: (u64, &str, Option<&str>), host: &str, here: bool) -> Value {
    json!({
        "organisation": { "slug": org.0, "name": org.1 },
        "project": { "id": project.0, "slug": project.1, "name": project.1, "codeHost": host, "codeHostRef": project.2, "repoId": 1 },
        "onThisComputer": here, "role": "admin"
    })
}

fn answer_of(rows: Vec<Value>) -> Value {
    json!({ "projects": rows })
}

fn listed(rows: Vec<Value>) -> Vec<CloudProject> {
    parse(&answer_of(rows)).unwrap()
}

#[test]
fn projects_are_grouped_by_organisation_with_retaskable_first_and_each_keeps_its_repo() {
    let projects = listed(vec![
        row(("acme", "Acme"), (1, "site", Some("Acme/site")), "github", false),
        row(
            ("jamez667", "jamez667"),
            (2, "toy", Some("jamez667/toy")),
            "github",
            false,
        ),
        row(
            ("retaskable", "Retaskable"),
            (3, "retaskable-com", Some("Retaskable/Retaskable.com")),
            "github",
            false,
        ),
        row(("acme", "Acme"), (4, "api", Some("Acme/api")), "github", false),
        row(
            ("retaskable", "Retaskable"),
            (5, "retaskable-agent", Some("Retaskable/retaskable-agent")),
            "github",
            false,
        ),
    ]);
    let groups = group(&projects, &[]);
    let shape: Vec<(String, Vec<String>)> = groups
        .iter()
        .map(|g| {
            (
                g["slug"].as_str().unwrap().to_string(),
                g["projects"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p["repo"].as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (
                "retaskable".to_string(),
                vec![
                    "Retaskable/Retaskable.com".to_string(),
                    "Retaskable/retaskable-agent".to_string()
                ]
            ),
            (
                "acme".to_string(),
                vec!["Acme/site".to_string(), "Acme/api".to_string()]
            ),
            ("jamez667".to_string(), vec!["jamez667/toy".to_string()]),
        ],
        "Retaskable first; the rest in the cloud's order"
    );
    assert_eq!(groups[0]["name"], "Retaskable");
    // Without Retaskable the cloud's order stands.
    let only = group(&projects[..2], &[]);
    assert_eq!(only[0]["slug"], "acme");
}

#[test]
fn a_project_is_on_this_computer_when_the_cloud_says_so_or_its_repo_is_in_this_agents_config() {
    let projects = listed(vec![
        row(("o", "O"), (1, "cloud-says", Some("o/cloud-says")), "github", true),
        row(("o", "O"), (2, "config-says", Some("O/Config-Says")), "github", false),
        row(("o", "O"), (3, "neither", Some("o/neither")), "github", false),
        row(("o", "O"), (4, "gitlab", None), "gitlab", false),
    ]);
    let groups = group(&projects, &["o/config-says".to_string()]);
    let marks: Vec<(bool, bool)> = groups[0]["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["onThisComputer"].as_bool().unwrap(), p["linkable"].as_bool().unwrap()))
        .collect();
    assert_eq!(marks, vec![(true, true), (true, true), (false, true), (false, false)]);
}

#[test]
fn a_malformed_answer_is_not_read_as_an_empty_list() {
    assert!(parse(&json!({ "projects": [{ "organisation": {} }] })).is_none());
    assert!(parse(&json!({})).is_none());
    assert_eq!(parse(&json!({ "projects": [] })), Some(vec![]));
}

#[test]
fn the_answer_falls_back_to_github_with_a_note_on_404_and_says_any_other_failure() {
    let fell = answer(Listed::NotDeployed, &[]);
    assert_eq!(
        (fell["source"].clone(), fell["note"].clone()),
        (json!("github"), json!(NOT_DEPLOYED))
    );
    let failed = answer(Listed::Failed("nope".into()), &[]);
    assert_eq!(
        (failed["source"].clone(), failed["error"].clone()),
        (json!("cloud"), json!("nope"))
    );
    let ok = answer(
        Listed::Projects(listed(vec![row(("o", "O"), (1, "p", Some("o/p")), "github", false)])),
        &[],
    );
    assert_eq!(
        (ok["source"].clone(), ok["groups"].as_array().unwrap().len()),
        (json!("cloud"), 1)
    );
}

struct Link(Mutex<Option<TokenAnswer>>);

impl CloudToken for Link {
    fn url(&self) -> &str {
        "cloud"
    }
    fn answer(&self) -> BoxFuture<'_, TokenAnswer> {
        async move { self.0.lock().unwrap().clone().unwrap() }.boxed()
    }
}

fn linked(base: &str) -> Link {
    Link(Mutex::new(Some(TokenAnswer::Token {
        url: base.into(),
        access_token: "tok".into(),
    })))
}

/// A cloud whose `GET /api/agents/projects` answers `status` with `body`, keeping the bearer it was asked with.
async fn cloud(status: u16, body: Value) -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let kept = seen.clone();
    let app = Router::new().route(
        "/api/agents/projects",
        get(move |headers: HeaderMap| {
            let (kept, body) = (kept.clone(), body.clone());
            async move {
                kept.lock()
                    .unwrap()
                    .push(headers["authorization"].to_str().unwrap().to_string());
                (Status::from_u16(status).unwrap(), axum::Json(body))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn the_list_is_asked_with_the_links_own_token_and_a_404_is_not_deployed() {
    let http = reqwest::Client::new();
    let rows = vec![row(
        ("retaskable", "Retaskable"),
        (3, "p", Some("Retaskable/p")),
        "github",
        false,
    )];
    let (base, seen) = cloud(200, answer_of(rows)).await;
    let Listed::Projects(projects) = fetch(&linked(&base), &http).await else {
        panic!("not listed")
    };
    assert_eq!(
        (projects[0].organisation_slug.as_str(), projects[0].repo.as_deref()),
        ("retaskable", Some("Retaskable/p"))
    );
    assert_eq!(*seen.lock().unwrap(), vec!["Bearer tok".to_string()]);

    let (base, _) = cloud(404, json!({ "error": "Not found" })).await;
    assert_eq!(fetch(&linked(&base), &http).await, Listed::NotDeployed);
    let (base, _) = cloud(401, json!({ "error": "Expected a valid access token." })).await;
    assert_eq!(
        fetch(&linked(&base), &http).await,
        Listed::Failed("retaskable.com refused the list (401): Expected a valid access token.".into())
    );
    let (base, _) = cloud(200, json!({ "nope": 1 })).await;
    assert!(matches!(fetch(&linked(&base), &http).await, Listed::Failed(_)));
    let idle = Link(Mutex::new(Some(TokenAnswer::Idle)));
    assert_eq!(
        fetch(&idle, &http).await,
        Listed::Failed("Link this agent to retaskable.com first.".into())
    );
    let down = Link(Mutex::new(Some(TokenAnswer::Unreachable)));
    assert!(matches!(fetch(&down, &http).await, Listed::Failed(w) if w.contains("Couldn't reach")));
}

struct Fixed(Listed);

impl ProjectSource for Fixed {
    fn list(&self) -> BoxFuture<'static, Listed> {
        let out = self.0.clone();
        async move { out }.boxed()
    }
}

struct Rig {
    dir: tempfile::TempDir,
    app: Router,
    cookie: String,
    csrf: String,
}

async fn rig(listed: Listed) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let first = dir.path().join("code").join("first");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::write(
        data.join("config.json"),
        json!({ "version": 2, "projects": [{ "id": "first", "repoPath": first, "githubRepo": "Retaskable/retaskable-agent", "baseBranch": "main" }] })
            .to_string(),
    )
    .unwrap();
    let settings = Arc::new(SettingsServer::new(
        data.clone(),
        4700,
        4700,
        false,
        Env::from_pairs([("GH_TOKEN", LEAKY)]),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(&data),
    ));
    let add = AddProject::with(
        data.clone(),
        Env::from_pairs([("GH_TOKEN", LEAKY)]),
        RepoPicker::with(
            Box::new(|| Ok(vec!["me".into()])),
            Box::new(|_| {
                Ok(vec![RepoSummary {
                    name_with_owner: "me/other".into(),
                    description: None,
                    visibility: "PUBLIC".into(),
                    is_fork: false,
                }])
            }),
            Box::new(std::time::Instant::now),
        ),
        Box::new(|_, dest| {
            make_repo(dest, None);
            Ok(())
        }),
        Box::new(|_| Some("main".into())),
        Box::new(|job| job()),
        Box::new(|| {}),
    );
    settings.set_add_project(add);
    settings.set_wizard_projects(Arc::new(Fixed(listed)));
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    Rig { dir, app, cookie, csrf }
}

impl Rig {
    async fn get(&self, path: &str) -> Value {
        let req = request("GET", path)
            .header("Cookie", &self.cookie)
            .body(Body::empty())
            .unwrap();
        json_body(self.app.clone().oneshot(req).await.unwrap()).await
    }

    async fn link(&self, body: Value) -> (Status, Value) {
        let req = request("POST", "/api/wizard/projects")
            .header("Origin", format!("http://{HOST}"))
            .header("Content-Type", "application/json")
            .header("Cookie", &self.cookie)
            .header("X-CSRF-Token", &self.csrf)
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        (res.status(), json_body(res).await)
    }
}

#[tokio::test]
async fn the_page_gets_the_groups_with_this_computers_own_projects_marked() {
    let rows = vec![
        row(("acme", "Acme"), (1, "site", Some("Acme/site")), "github", false),
        row(
            ("retaskable", "Retaskable"),
            (2, "retaskable-agent", Some("Retaskable/retaskable-agent")),
            "github",
            false,
        ),
    ];
    let r = rig(Listed::Projects(listed(rows))).await;
    let got = r.get("/api/wizard/projects").await;
    assert_eq!(got["source"], "cloud");
    assert_eq!(got["groups"][0]["slug"], "retaskable");
    assert_eq!(
        got["groups"][0]["projects"][0]["onThisComputer"], true,
        "from config.json"
    );
    assert_eq!(got["groups"][1]["projects"][0]["onThisComputer"], false);
}

#[tokio::test]
async fn a_cloud_without_the_route_falls_back_to_the_github_list_with_a_note() {
    let r = rig(Listed::NotDeployed).await;
    let got = r.get("/api/wizard/projects").await;
    assert_eq!(
        (got["source"].clone(), got["note"].clone()),
        (json!("github"), json!(NOT_DEPLOYED))
    );
    // The GitHub picker the page falls back to is today's, unchanged.
    assert_eq!(r.get("/api/projects/owners").await["owners"], json!(["me"]));
}

#[tokio::test]
async fn ticking_a_project_links_its_repo_into_its_organisation() {
    let r = rig(Listed::Projects(vec![])).await;
    let (status, body) = r
        .link(json!({ "repo": "Retaskable/Retaskable.com", "organisation": "retaskable" }))
        .await;
    assert_eq!((status, body), (Status::ACCEPTED, json!({ "adding": true })));
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(r.dir.path().join("data/config.json")).unwrap()).unwrap();
    let added = config["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["githubRepo"] == "Retaskable/Retaskable.com")
        .unwrap();
    assert_eq!(added["organisationSlug"], "retaskable");
    // Already on this computer, or no JSON, is refused as Add project refuses it.
    let (status, _) = r
        .link(json!({ "repo": "Retaskable/Retaskable.com", "organisation": "retaskable" }))
        .await;
    assert_eq!(status, Status::CONFLICT);
    assert_eq!(r.link(json!("nope")).await.0, Status::BAD_REQUEST);
}
