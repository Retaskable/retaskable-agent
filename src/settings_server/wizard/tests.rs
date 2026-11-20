//! Each wizard step's check (docs/specs/setup-wizard.md section 6): the pure logic with facts built by hand, and the
//! routes over HTTP with GitHub and Claude made up, including the save and read round trip from an empty data folder.
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::probes::Probes;
use super::steps::*;
use crate::env::Env;
use crate::secrets::fakes::{FakeHost, MemStore};
use crate::secrets::{KEY_FILE_VAR, Secrets};
use crate::settings_server::test_support::*;
use crate::settings_server::{SettingsServer, StatusCode};

const TOKEN: &str = "ghp_PLANTED_wizard_token_1234567890";
const GH_REPLY: &str = "HTTP/2.0 200 OK\r\nX-Oauth-Scopes: repo, workflow, read:org\r\n\r\n{\"login\":\"octo\",\"name\":\"The Octocat\",\"id\":7,\"email\":null}";

// ---- The steps as logic ----

fn done_facts() -> Facts {
    Facts {
        computer: "box".into(),
        can_act: true,
        storage_problem: None,
        links: LinkFacts {
            cloud_name: "Production".into(),
            cloud: true,
            store: true,
            runner: Some(true),
            ..LinkFacts::default()
        },
        github: GithubFacts {
            token: Token::Saved,
            check: Some(Ok(GhOk {
                login: "octo".into(),
                name: Some("The Octocat".into()),
                identity: Some("The Octocat <7+octo@users.noreply.github.com>".into()),
            })),
        },
        claude: ClaudeFacts {
            saved: Some("CLAUDE_CODE_OAUTH_TOKEN"),
            status: ClaudeStatus::SignedIn("Claude Code is signed in".into()),
        },
        projects: ProjectFacts {
            on_computer: vec![ProjectRow {
                id: "p".into(),
                repo: "o/p".into(),
                paused: None,
            }],
            ..ProjectFacts::default()
        },
        waiting: vec![],
        ci: CiFacts {
            state: "running".into(),
            linked: true,
            token: Some("the code agent's token".into()),
            docker: Some("ok".into()),
            ..CiFacts::default()
        },
    }
}

fn state_of(f: &Facts, id: &str) -> (State, Step) {
    let all = steps(f);
    let step = all.into_iter().find(|s| s.id == id).unwrap();
    (step.state, step)
}

#[test]
fn everything_in_place_is_complete_and_has_no_current_step() {
    let all = steps(&done_facts());
    assert!(complete(&all));
    assert_eq!(current(&all), None);
    assert_eq!(all.len(), 7);
}

#[test]
fn link_needs_each_approval_and_says_how_many_are_left() {
    let mut f = done_facts();
    f.links = LinkFacts {
        cloud_name: "Production".into(),
        runner: Some(false),
        ..LinkFacts::default()
    };
    let (state, step) = state_of(&f, "link");
    assert_eq!(state, State::Needs);
    let todo = step.todo.unwrap();
    assert!(todo.contains("3 approvals") && todo.contains("this computer") && todo.contains("the CI runner"));
    // One of three done, the code waiting on the page.
    f.links.cloud = true;
    f.links.waiting = Some(Approve {
        label: "the shared backlog".into(),
        url: Some("https://store.example/link?code=AB".into()),
        code: "AB-12".into(),
    });
    let (_, step) = state_of(&f, "link");
    assert!(step.todo.as_deref().unwrap().contains("AB-12") && step.todo_has_left(2));
    assert_eq!(step.data["approve"]["url"], "https://store.example/link?code=AB");
    // The runner has none to link when CI is off.
    f.links = LinkFacts {
        cloud: true,
        store: true,
        runner: None,
        ..LinkFacts::default()
    };
    assert_eq!(state_of(&f, "link").0, State::Done);
    f.links.store = false;
    f.links.error = Some("The store said no".into());
    let (state, step) = state_of(&f, "link");
    assert_eq!(
        (state, step.failed.as_deref()),
        (State::Failed, Some("The store said no"))
    );
}

trait Left {
    fn todo_has_left(&self, n: usize) -> bool;
}
impl Left for Step {
    fn todo_has_left(&self, n: usize) -> bool {
        self.todo
            .as_deref()
            .is_some_and(|t| t.contains(&format!("{n} approval")))
    }
}

#[test]
fn github_is_done_with_the_account_failed_with_why_and_needs_a_token_when_there_is_none() {
    let mut f = done_facts();
    let (state, step) = state_of(&f, "github");
    assert_eq!(state, State::Done);
    assert!(step.detail.contains("The Octocat (octo)") && step.detail.contains("Commits are made as"));
    f.github.check = Some(Err("GitHub refused the token (HTTP 401)".into()));
    let (state, step) = state_of(&f, "github");
    assert_eq!(
        (state, step.failed.as_deref()),
        (State::Failed, Some("GitHub refused the token (HTTP 401)"))
    );
    f.github = GithubFacts {
        token: Token::Unset,
        check: None,
    };
    let (state, step) = state_of(&f, "github");
    assert_eq!(state, State::Needs);
    assert!(step.todo.unwrap().contains("Create a token"));
    f.github.token = Token::Unreadable;
    assert_eq!(state_of(&f, "github").0, State::Failed);
    f.storage_problem = Some("can't write /data".into());
    assert_eq!(state_of(&f, "github").1.failed.as_deref(), Some("can't write /data"));
}

#[test]
fn gh_output_is_read_for_the_account_and_a_classic_tokens_scopes() {
    let (user, scopes) = parse_gh_user(GH_REPLY).unwrap();
    assert_eq!(user["login"], "octo");
    assert_eq!(
        scopes.as_deref(),
        Some(&["repo".to_string(), "workflow".into(), "read:org".into()][..])
    );
    assert_eq!(missing_scopes(scopes.as_deref().unwrap()), None);
    let why = missing_scopes(&["repo".to_string()]).unwrap();
    assert!(why.contains("workflow") && !why.contains("repo and"));
    assert!(missing_scopes(&[]).unwrap().contains("repo and workflow scopes"));
    // A fine-grained token has no scopes header to read.
    let (_, none) = parse_gh_user("HTTP/2.0 200 OK\n\n{\"login\":\"octo\"}").unwrap();
    assert_eq!(none, None);
    assert!(parse_gh_user("HTTP/2.0 200 OK\n\n{\"message\":\"x\"}").is_err());
    assert!(parse_gh_user("garbage").is_err());
}

#[test]
fn claude_is_done_when_signed_in_and_says_how_to_get_a_token_when_not() {
    let mut f = done_facts();
    assert_eq!(state_of(&f, "claude").0, State::Done);
    f.claude = ClaudeFacts {
        saved: None,
        status: ClaudeStatus::SignedOut,
    };
    let (state, step) = state_of(&f, "claude");
    assert_eq!(state, State::Needs);
    assert!(step.todo.unwrap().contains("claude setup-token"));
    f.claude.saved = Some("ANTHROPIC_API_KEY");
    assert_eq!(state_of(&f, "claude").0, State::Failed);
    f.claude.status = ClaudeStatus::Missing;
    assert!(state_of(&f, "claude").1.failed.unwrap().contains("isn't installed"));
    f.claude.status = ClaudeStatus::Unknown("Couldn't tell".into());
    assert_eq!(state_of(&f, "claude").1.failed.as_deref(), Some("Couldn't tell"));
}

#[test]
fn projects_need_one_on_this_computer_and_report_an_add_under_way_or_failed() {
    let mut f = done_facts();
    assert_eq!(state_of(&f, "projects").1.detail, "On this computer: o/p.");
    f.projects.on_computer.clear();
    assert_eq!(state_of(&f, "projects").0, State::Needs);
    f.projects.adding = Some(("o/q".into(), "cloning".into()));
    assert!(
        state_of(&f, "projects")
            .1
            .todo
            .unwrap()
            .contains("Linking o/q: cloning")
    );
    f.projects.adding = None;
    f.projects.add_failed = Some(("o/q".into(), "not found".into()));
    let (state, step) = state_of(&f, "projects");
    assert_eq!(state, State::Failed);
    assert!(step.failed.unwrap().contains("o/q") && step.data["adding"].is_null());
}

#[test]
fn commands_wait_for_the_owner_on_the_site_and_the_step_passes_when_none_do() {
    let mut f = done_facts();
    assert_eq!(state_of(&f, "commands").0, State::Done);
    f.waiting = vec![WaitingCommands {
        project: "o/p".into(),
        text: "checks: `npm test` → `npm run e2e`".into(),
        by: Some("sam".into()),
    }];
    let (state, step) = state_of(&f, "commands");
    assert_eq!(state, State::Needs);
    assert!(step.todo.unwrap().contains("o/p"));
    assert_eq!(step.data["waiting"][0]["text"], "checks: `npm test` → `npm run e2e`");
}

#[test]
fn ci_checks_the_runner_its_link_its_token_and_docker_and_can_be_off() {
    let mut f = done_facts();
    f.ci.last_check = Some("Last check: PR #3 on o/p passed in 40 s, just now.".into());
    let (state, step) = state_of(&f, "ci");
    assert_eq!(state, State::Done);
    assert!(step.detail.contains("PR #3") && step.detail.contains("code agent's token"));
    f.ci.docker = Some("missing".into());
    assert_eq!(state_of(&f, "ci").0, State::Failed);
    f.ci.docker = None;
    assert_eq!(state_of(&f, "ci").0, State::Needs);
    f.ci.docker = Some("ok".into());
    f.ci.token = None;
    assert!(state_of(&f, "ci").1.todo.unwrap().contains("GitHub token"));
    f.ci.token = Some("x".into());
    f.ci.linked = false;
    assert!(state_of(&f, "ci").1.todo.unwrap().contains("step 1"));
    f.ci.state = "stopped".into();
    assert_eq!(state_of(&f, "ci").0, State::Failed);
    f.ci.state = "notInstalled".into();
    assert_eq!(state_of(&f, "ci").0, State::Needs);
    f.ci.off = true;
    let (state, step) = state_of(&f, "ci");
    assert_eq!(state, State::Done);
    assert!(step.detail.starts_with("Off"));
}

#[test]
fn done_says_plainly_when_every_project_is_still_paused() {
    let mut f = done_facts();
    assert!(
        state_of(&f, "done").1.detail.contains("is set up"),
        "working: the plain done"
    );
    assert!(!state_of(&f, "done").1.detail.contains("paused"));
    f.projects.on_computer[0].paused = Some("Paused when it was added".into());
    let (state, done) = state_of(&f, "done");
    assert_eq!(state, State::Done, "setup is done, it just isn't working yet");
    assert!(done.detail.contains("every project is paused"), "{}", done.detail);
    assert_eq!(done.data["allPaused"], true);
    let (_, projects) = state_of(&f, "projects");
    assert!(projects.detail.contains("Start working on it"), "{}", projects.detail);
    assert_eq!(projects.data["onComputer"][0]["paused"], "Paused when it was added");
    // One working project is enough.
    f.projects.on_computer.push(ProjectRow {
        id: "q".into(),
        repo: "o/q".into(),
        paused: None,
    });
    assert!(!state_of(&f, "done").1.detail.contains("every project is paused"));
}

#[test]
fn done_waits_for_every_step_and_current_is_the_first_that_is_not() {
    let mut f = done_facts();
    f.claude.status = ClaudeStatus::SignedOut;
    f.claude.saved = None;
    f.ci.state = "stopped".into();
    let all = steps(&f);
    assert!(!complete(&all));
    assert_eq!(current(&all), Some("claude"));
    assert_eq!(all.last().unwrap().state, State::Needs);
}

#[test]
fn the_last_check_is_said_in_words() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(super::gather::last_check_for_test(dir.path(), 0), None);
    std::fs::write(
        dir.path().join("last-check.json"),
        r#"{"at":1000000,"repo":"o/p","subject":"PR #3","passed":false,"ms":41000}"#,
    )
    .unwrap();
    let said = super::gather::last_check_for_test(dir.path(), 1_000_000 + 5 * 60_000).unwrap();
    assert_eq!(said, "Last check: PR #3 on o/p failed in 41 s, 5 minutes ago.");
}

// ---- The routes ----

struct Stub {
    gh: Mutex<Result<String, String>>,
    claude: Mutex<ClaudeStatus>,
    test: Mutex<Result<String, String>>,
    asked: Mutex<Vec<&'static str>>,
}

impl Stub {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            gh: Mutex::new(Ok(GH_REPLY.to_string())),
            claude: Mutex::new(ClaudeStatus::SignedOut),
            test: Mutex::new(Ok("ok".into())),
            asked: Mutex::new(vec![]),
        })
    }
}

impl Probes for Stub {
    fn gh_user(&self) -> BoxFuture<'static, Result<String, String>> {
        self.asked.lock().unwrap().push("gh_user");
        let out = self.gh.lock().unwrap().clone();
        Box::pin(async move { out })
    }
    fn gh_repos(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn claude(&self) -> BoxFuture<'static, ClaudeStatus> {
        let out = self.claude.lock().unwrap().clone();
        Box::pin(async move { out })
    }
    fn claude_test(&self) -> BoxFuture<'static, Result<String, String>> {
        let out = self.test.lock().unwrap().clone();
        Box::pin(async move { out })
    }
    fn identity(&self, _user: Value) -> Option<String> {
        Some("The Octocat <7+octo@users.noreply.github.com>".into())
    }
}

struct Rig {
    dir: tempfile::TempDir,
    settings: Arc<SettingsServer>,
    app: Router,
    cookie: String,
    csrf: String,
    stub: Arc<Stub>,
}

fn key_env(dir: &std::path::Path) -> Env {
    Env::from_pairs([(KEY_FILE_VAR, dir.join("secrets.key").to_str().unwrap())])
}

fn secrets_in(dir: &std::path::Path) -> Arc<Secrets> {
    Arc::new(Secrets::new(
        dir,
        &key_env(dir),
        Arc::new(FakeHost::default()),
        None,
        Arc::new(MemStore::default()),
    ))
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    settings.set_secrets(secrets_in(dir.path()));
    let stub = Stub::new();
    settings.set_wizard_probes(stub.clone());
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    Rig {
        dir,
        settings,
        app,
        cookie,
        csrf,
        stub,
    }
}

impl Rig {
    async fn get(&self, path: &str) -> Value {
        let res = self
            .app
            .clone()
            .oneshot(
                request("GET", path)
                    .header("Cookie", &self.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        json_body(res).await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.post_via(&self.app, path, body).await
    }

    async fn post_via(&self, app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
        let res = app
            .clone()
            .oneshot(
                request("POST", path)
                    .header("Origin", format!("http://{HOST}"))
                    .header("Content-Type", "application/json")
                    .header("Cookie", &self.cookie)
                    .header("X-CSRF-Token", &self.csrf)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        (res.status(), json_body(res).await)
    }
}

fn step<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn a_wiped_computer_shows_every_step_needing_the_person_and_asks_for_a_sign_in() {
    let r = rig().await;
    // No token saved and `gh` isn't signed in either.
    *r.stub.gh.lock().unwrap() = Err("gh isn't signed in".into());
    let view = r.get("/api/wizard").await;
    assert_eq!(view["complete"], false);
    assert_eq!(view["current"], "link");
    assert_eq!(step(&view, "link")["state"], "needs");
    assert_eq!(step(&view, "link")["data"]["target"], "production");
    assert_eq!(step(&view, "github")["state"], "needs");
    assert_eq!(step(&view, "claude")["state"], "needs");
    assert_eq!(step(&view, "projects")["state"], "needs");
    // The runner isn't there at all: CI needs the person, with the way to turn it off.
    assert_eq!(step(&view, "ci")["state"], "needs");
    let res = app(r.settings.clone())
        .oneshot(request("GET", "/api/wizard").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_saved_github_token_makes_the_files_reads_back_and_shows_the_account() {
    let r = rig().await;
    let (status, body) = r.post("/api/wizard/github", json!({ "token": TOKEN })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["step"]["state"], "done");
    assert!(body["step"]["detail"].as_str().unwrap().contains("The Octocat (octo)"));
    assert!(!body.to_string().contains(TOKEN), "a token is never in an answer");
    // The files the 2026-11-19 save never made.
    let key = r.dir.path().join("secrets.key");
    let config = std::fs::read_to_string(r.dir.path().join("config.json")).unwrap();
    assert!(key.exists() && config.contains("\"GH_TOKEN\"") && config.contains("v1:") && !config.contains(TOKEN));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // A second program (a restart) opens it with the same key, from the files alone.
    let again = secrets_in(r.dir.path());
    let state = again.states().into_iter().find(|s| s.name == "GH_TOKEN").unwrap();
    assert_eq!(state.source, crate::secrets::Source::Saved);
    assert_eq!(state.fingerprint, Some(crate::secrets::fingerprint(TOKEN)));
    // And the wizard now says done, with the account.
    let view = r.get("/api/wizard").await;
    assert_eq!(step(&view, "github")["state"], "done");
    assert_eq!(step(&view, "github")["data"]["login"], "octo");
}

#[tokio::test]
async fn a_token_github_refuses_is_saved_and_the_step_says_why() {
    let r = rig().await;
    *r.stub.gh.lock().unwrap() = Err("GitHub refused the token (HTTP 401): it is wrong.".into());
    let (status, body) = r.post("/api/wizard/github", json!({ "token": TOKEN })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["step"]["state"], "failed");
    assert!(body["step"]["failed"].as_str().unwrap().contains("HTTP 401"));
    // A classic token without `workflow` is named.
    *r.stub.gh.lock().unwrap() = Ok("HTTP/2.0 200 OK\nX-OAuth-Scopes: repo\n\n{\"login\":\"octo\"}".into());
    let (_, body) = r.post("/api/wizard/github", json!({ "token": TOKEN })).await;
    assert!(body["step"]["failed"].as_str().unwrap().contains("workflow"));
}

#[tokio::test]
async fn a_blank_or_spaced_token_is_refused_and_nothing_is_written() {
    let r = rig().await;
    for bad in ["", "has a space inside"] {
        let (status, body) = r.post("/api/wizard/github", json!({ "token": bad })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert!(!r.dir.path().join("config.json").exists() && !r.dir.path().join("secrets.key").exists());
}

#[tokio::test]
async fn saving_is_refused_from_another_address_and_writes_nothing() {
    let r = rig().await;
    let other = app_from(r.settings.clone(), IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)));
    let (status, _) = r
        .post_via(&other, "/api/wizard/github", json!({ "token": TOKEN }))
        .await;
    // Another computer's request doesn't even reach the route: the envelope answers 404 (agent-settings.md section 4.3).
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!r.dir.path().join("config.json").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_data_folder_that_cannot_be_written_is_said_not_swallowed() {
    use std::os::unix::fs::PermissionsExt;
    let r = rig().await;
    std::fs::set_permissions(r.dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let writable = std::fs::write(r.dir.path().join("probe"), b"x").is_ok();
    let (status, body) = r.post("/api/wizard/github", json!({ "token": TOKEN })).await;
    let view = r.get("/api/wizard").await;
    std::fs::set_permissions(r.dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    if writable {
        return; // running as root: nothing can be made unwritable
    }
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("can't write to its data folder")
    );
    assert!(view["storageProblem"].as_str().unwrap().contains("data folder"));
    assert_eq!(step(&view, "github")["state"], "failed");
}

#[tokio::test]
async fn claude_is_saved_as_a_token_or_a_key_and_tested_with_a_tiny_call() {
    let r = rig().await;
    let (status, _) = r
        .post(
            "/api/wizard/claude",
            json!({ "token": "sk-ant-oat-x", "kind": "weird" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = r
        .post(
            "/api/wizard/claude",
            json!({ "token": "sk-ant-oat-planted", "kind": "token" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // Signed out still: the saved token wasn't accepted.
    assert_eq!(body["step"]["state"], "failed");
    *r.stub.claude.lock().unwrap() = ClaudeStatus::SignedIn("Claude Code is signed in".into());
    let (_, body) = r
        .post(
            "/api/wizard/claude",
            json!({ "token": "sk-ant-api-planted", "kind": "key" }),
        )
        .await;
    assert_eq!(body["step"]["state"], "done");
    let config = std::fs::read_to_string(r.dir.path().join("config.json")).unwrap();
    assert!(
        config.contains("CLAUDE_CODE_OAUTH_TOKEN")
            && config.contains("ANTHROPIC_API_KEY")
            && !config.contains("planted")
    );
    let (_, said) = r.post("/api/wizard/claude/test", json!({})).await;
    assert_eq!(said, json!({ "ok": true, "said": "ok" }));
    *r.stub.test.lock().unwrap() = Err("Claude Code didn't answer: 401".into());
    let (_, said) = r.post("/api/wizard/claude/test", json!({})).await;
    assert_eq!(said["ok"], false);
}

#[tokio::test]
async fn ci_is_turned_off_and_on_in_the_runners_own_settings() {
    let r = rig().await;
    let ci = r.dir.path().join("ci");
    std::fs::create_dir_all(&ci).unwrap();
    // The runner has written its status, so it counts as installed.
    let now_ms = ((r.settings.now)() * 1000).to_string();
    std::fs::write(
        ci.join("status.json"),
        format!(
            r#"{{"program":"ci-runner","version":"1","pid":1,"startedAt":{now_ms},"updatedAt":{now_ms},"linked":{{"store":true}},"envSet":[],"docker":"ok"}}"#
        ),
    )
    .unwrap();
    let (status, body) = r.post("/api/wizard/ci", json!({ "off": true })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let file = std::fs::read_to_string(ci.join("settings.json")).unwrap();
    assert!(file.contains("\"slots\": 0"), "{file}");
    let view = r.get("/api/wizard").await;
    assert_eq!(step(&view, "ci")["state"], "done");
    assert!(step(&view, "ci")["detail"].as_str().unwrap().starts_with("Off"));
    let (status, _) = r.post("/api/wizard/ci", json!({ "off": false })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !std::fs::read_to_string(ci.join("settings.json"))
            .unwrap_or_default()
            .contains("\"slots\"")
    );
}

#[tokio::test]
async fn the_first_visit_door_is_open_only_from_the_docker_gateway_and_only_until_setup_is_done() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = settings_with_network(dir.path(), false, None);
    let gateway = IpAddr::V4(Ipv4Addr::new(172, 17, 0, 1));
    Arc::get_mut(&mut Arc::get_mut(&mut settings).unwrap().host)
        .unwrap()
        .trusted_gateway = Some(gateway);
    let visit = |from: IpAddr| {
        let app = app_from(settings.clone(), from);
        async move {
            app.oneshot(post_body("/api/settings/session", r#"{"firstVisit":true}"#))
                .await
                .unwrap()
        }
    };
    // A wiped computer: the gateway gets in with no link, a loopback visitor does not.
    let res = visit(gateway).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().contains_key("set-cookie"));
    assert_eq!(
        visit(Ipv4Addr::LOCALHOST.into()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_ne!(visit(Ipv4Addr::new(10, 0, 0, 5).into()).await.status(), StatusCode::OK);
    // Once a visit found every step done, the door shuts, and a refusal wrongly counts against nobody.
    std::fs::write(dir.path().join("wizard.json"), r#"{"complete":true}"#).unwrap();
    for _ in 0..8 {
        assert_eq!(visit(gateway).await.status(), StatusCode::UNAUTHORIZED);
    }
    let token = mint_token(&settings, 300);
    let app = app_from(settings.clone(), gateway);
    let res = app
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": token }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "the link still signs in");
    // A wipe removes the note: the door opens again.
    std::fs::remove_file(dir.path().join("wizard.json")).unwrap();
    assert_eq!(visit(gateway).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn without_the_docker_opt_in_there_is_no_door_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings);
    let res = app
        .oneshot(post_body("/api/settings/session", r#"{"firstVisit":true}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_visit_that_finds_everything_done_notes_it_and_a_later_gap_reopens_the_door() {
    let r = rig().await;
    assert!(r.settings.wizard_incomplete());
    r.settings.note_complete(true);
    assert!(!r.settings.wizard_incomplete());
    r.settings.note_complete(false);
    assert!(r.settings.wizard_incomplete());
}
