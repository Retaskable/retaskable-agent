//! The programs panel and the other programs' saves (agent-settings.md section 4.6): states from status files with an
//! injected clock, locks from `envSet`, a save writing only that program's file, and the same session, CSRF, Origin
//! and body-limit checks as every other write.
use std::path::Path;

use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::page_host::ProgramUpdate;
use crate::program_status::{self, Linked, Report, StatusWriter};
use crate::secrets::fakes::MemStore;

/// The test server's clock, in ms (`test_server` answers 1_700_000_000 s).
const NOW: u64 = 1_700_000_000_000;
const TOKEN: &str = "ghp_PLANTED_ci_token_1234567890";

/// A status file for `program` in `folder`, written at `at` ms.
fn write_status(folder: &Path, program: &'static str, at: u64, store: bool, env_set: &[&str], error: Option<&str>) {
    StatusWriter::new(folder, program, "1.2.3", at)
        .write(
            at,
            Report {
                linked: Linked { store, cloud: None },
                last_error: error.map(str::to_string),
                env_set: env_set.iter().map(|s| s.to_string()).collect(),
                ..Report::default()
            },
        )
        .unwrap();
}

fn ci_dir(root: &Path) -> std::path::PathBuf {
    crate::paths::ci_dir(root)
}

async fn get_programs(app: &Router, cookie: &str) -> Value {
    let res = app
        .clone()
        .oneshot(
            request("GET", "/api/settings/programs")
                .header("Cookie", cookie.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await
}

async fn post_program(app: &Router, cookie: &str, csrf: &str, program: &str, body: Value) -> Response {
    app.clone()
        .oneshot(
            request("POST", &format!("/api/settings/programs/{program}"))
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie.to_string())
                .header("X-CSRF-Token", csrf.to_string())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

fn program<'a>(all: &'a Value, id: &str) -> &'a Value {
    all["programs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn the_panel_needs_a_session() {
    let dir = tempfile::tempdir().unwrap();
    let (_, app, _, _) = signed_in_app(dir.path()).await;
    let res = app
        .oneshot(request("GET", "/api/settings/programs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn each_program_shows_its_state_from_its_status_file_and_the_injected_clock() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(root, program_status::CODE_AGENT, NOW - 5_000, true, &[], None);
    write_status(
        &ci_dir(root),
        program_status::CI_RUNNER,
        NOW - 60_000,
        true,
        &[],
        Some("slots must be a number"),
    );
    let (_, app, cookie, _) = signed_in_app(root).await;
    let all = get_programs(&app, &cookie).await;
    let code = program(&all, "code-agent");
    assert_eq!(
        (code["state"].clone(), code["settings"].clone()),
        (json!("running"), Value::Null)
    );
    let ci = program(&all, "ci-runner");
    assert_eq!(ci["state"], "stopped");
    assert_eq!(ci["lastSeen"], NOW - 60_000);
    assert_eq!(ci["lastError"], "slots must be a number");
    let chat = program(&all, "chat-agent");
    assert_eq!(chat["state"], "notInstalled");
    assert_eq!(
        (chat["settings"].clone(), chat["secrets"].clone()),
        (Value::Null, Value::Null)
    );
}

#[tokio::test]
async fn an_unlinked_program_shows_its_cli_link_command_and_a_linked_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(
        &crate::paths::chat_dir(root),
        program_status::CHAT_AGENT,
        NOW - 1_000,
        false,
        &[],
        None,
    );
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW - 1_000, true, &[], None);
    let (_, app, cookie, _) = signed_in_app(root).await;
    let all = get_programs(&app, &cookie).await;
    let chat = program(&all, "chat-agent");
    assert_eq!(
        (chat["state"].clone(), chat["linkCommand"].clone()),
        (json!("unlinked"), json!("retaskable-chat-agent link"))
    );
    assert_eq!(program(&all, "ci-runner")["linkCommand"], Value::Null);
    assert!(
        !all.to_string().contains("linkUrl"),
        "no link button, only a command to run"
    );
}

#[tokio::test]
async fn a_programs_settings_come_from_its_own_file_with_the_fields_its_environment_sets_locked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(
        &ci_dir(root),
        program_status::CI_RUNNER,
        NOW,
        true,
        &["AGENT_CI_SLOTS"],
        None,
    );
    std::fs::write(
        ci_dir(root).join("settings.json"),
        r#"{"repos":["a/b"],"slots":3,"cpus":4}"#,
    )
    .unwrap();
    let (_, app, cookie, _) = signed_in_app(root).await;
    let ci = get_programs(&app, &cookie).await;
    let settings = &program(&ci, "ci-runner")["settings"];
    assert_eq!(settings["file"]["repos"], json!(["a/b"]));
    assert_eq!(settings["file"]["slots"], 3);
    assert_eq!(settings["locked"], json!(["slots"]));
    assert_eq!(settings["error"], Value::Null);
    // A bad file is reported, not hidden.
    std::fs::write(ci_dir(root).join("settings.json"), "{oops").unwrap();
    let ci = get_programs(&app, &cookie).await;
    assert!(
        program(&ci, "ci-runner")["settings"]["error"]
            .as_str()
            .unwrap()
            .contains("settings.json")
    );
}

#[tokio::test]
async fn a_save_writes_only_that_programs_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW, true, &[], None);
    write_status(
        &crate::paths::chat_dir(root),
        program_status::CHAT_AGENT,
        NOW,
        true,
        &[],
        None,
    );
    std::fs::write(root.join("config.json"), r#"{"version":2,"projects":[]}"#).unwrap();
    let (_, app, cookie, csrf) = signed_in_app(root).await;
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "ci-runner",
        json!({"settings": {"repos": ["o/p@dev"], "slots": 5}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(ci_dir(root).join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        (saved["repos"].clone(), saved["slots"].clone()),
        (json!(["o/p@dev"]), json!(5))
    );
    assert_eq!(
        std::fs::read_to_string(root.join("config.json")).unwrap(),
        r#"{"version":2,"projects":[]}"#
    );
    assert!(!crate::paths::chat_dir(root).join("settings.json").exists());
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "chat-agent",
        json!({"settings": {"onlyRepoId": "42"}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        std::fs::read_to_string(crate::paths::chat_dir(root).join("settings.json"))
            .unwrap()
            .contains("42")
    );
}

#[tokio::test]
async fn a_field_the_programs_environment_sets_is_refused_and_nothing_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(
        &ci_dir(root),
        program_status::CI_RUNNER,
        NOW,
        true,
        &["AGENT_CI_REPO"],
        None,
    );
    let (_, app, cookie, csrf) = signed_in_app(root).await;
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "ci-runner",
        json!({"settings": {"slots": 3, "repos": ["o/p"]}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        json_body(res).await["error"]
            .as_str()
            .unwrap()
            .contains("AGENT_CI_REPO")
    );
    assert!(!ci_dir(root).join("settings.json").exists());
}

#[tokio::test]
async fn a_bad_save_is_a_400_and_the_code_agent_unknown_and_absent_programs_are_not_saved_here() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW, true, &[], None);
    let (_, app, cookie, csrf) = signed_in_app(root).await;
    for (program, body, want) in [
        (
            "ci-runner",
            json!({"settings": {"slots": "many"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "ci-runner",
            json!({"settings": {"image": "x"}}),
            StatusCode::BAD_REQUEST,
        ),
        ("ci-runner", json!({"other": 1}), StatusCode::BAD_REQUEST),
        (
            "ci-runner",
            json!({"secrets": {"CLOUDFLARE_API_TOKEN": "x"}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "chat-agent",
            json!({"settings": {"onlyRepoId": "1"}}),
            StatusCode::BAD_REQUEST,
        ),
        ("code-agent", json!({"settings": {}}), StatusCode::NOT_FOUND),
        ("nope", json!({}), StatusCode::NOT_FOUND),
    ] {
        let res = post_program(&app, &cookie, &csrf, program, body.clone()).await;
        assert_eq!(res.status(), want, "{program} {body}");
    }
    assert!(!ci_dir(root).join("settings.json").exists());
    assert!(!crate::paths::chat_dir(root).exists());
}

#[tokio::test]
async fn a_save_needs_the_session_csrf_origin_and_a_small_body() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW, true, &[], None);
    let (_, app, cookie, csrf) = signed_in_app(root).await;
    let url = "/api/settings/programs/ci-runner";
    let body = json!({"settings": {"slots": 1}}).to_string();
    let send = |req: axum::http::request::Builder| {
        let (app, body) = (app.clone(), body.clone());
        async move { app.oneshot(req.body(Body::from(body)).unwrap()).await.unwrap().status() }
    };
    let origin = format!("http://{HOST}");
    // No session.
    assert_eq!(
        send(request("POST", url).header("Origin", &origin)).await,
        StatusCode::UNAUTHORIZED
    );
    // A session without the CSRF token, and with a wrong one.
    let no_csrf = request("POST", url).header("Origin", &origin).header("Cookie", &cookie);
    assert_eq!(send(no_csrf).await, StatusCode::UNAUTHORIZED);
    let bad_csrf = request("POST", url)
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", "nope");
    assert_eq!(send(bad_csrf).await, StatusCode::UNAUTHORIZED);
    // Another origin or none: the same 404 as a path that doesn't exist.
    let other = request("POST", url)
        .header("Origin", "http://evil.example")
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", &csrf);
    assert_eq!(send(other).await, StatusCode::NOT_FOUND);
    assert_eq!(
        send(
            request("POST", url)
                .header("Cookie", &cookie)
                .header("X-CSRF-Token", &csrf)
        )
        .await,
        StatusCode::NOT_FOUND
    );
    // A Host that isn't this computer's own.
    let bad_host = Request::builder()
        .method("POST")
        .uri(url)
        .header("Host", "evil.example")
        .header("Origin", "http://evil.example")
        .header("Cookie", &cookie)
        .header("X-CSRF-Token", &csrf);
    assert_eq!(send(bad_host).await, StatusCode::NOT_FOUND);
    // More than 16 KB.
    let big = json!({"settings": {"name": "x".repeat(20_000)}}).to_string();
    let res = app
        .clone()
        .oneshot(
            request("POST", url)
                .header("Origin", &origin)
                .header("Cookie", &cookie)
                .header("X-CSRF-Token", &csrf)
                .body(Body::from(big))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!ci_dir(root).join("settings.json").exists());
}

#[tokio::test]
async fn a_secret_is_sealed_into_that_programs_file_never_shown_and_never_set_in_this_process() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW, true, &[], None);
    let (settings, app, cookie, csrf) = signed_in_app(root).await;
    settings.set_program_keys(Some(Arc::new(MemStore::default())));
    let before = std::env::var("GH_TOKEN").ok();
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "ci-runner",
        json!({"secrets": {"GH_TOKEN": TOKEN}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(!json_body(res).await.to_string().contains(TOKEN));
    assert_eq!(
        std::env::var("GH_TOKEN").ok(),
        before,
        "the page's own environment is untouched"
    );
    let file = std::fs::read_to_string(ci_dir(root).join("settings.json")).unwrap();
    assert!(file.contains("v1:") && !file.contains(TOKEN));
    let all = get_programs(&app, &cookie).await;
    assert!(!all.to_string().contains(TOKEN));
    let ci = program(&all, "ci-runner");
    assert_eq!(ci["secrets"][0]["name"], "GH_TOKEN");
    assert_eq!(ci["secrets"][0]["source"], "saved");
    assert_eq!(ci["canSaveSecrets"], true);
    // Clearing it.
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "ci-runner",
        json!({"secrets": {"GH_TOKEN": null}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        !std::fs::read_to_string(ci_dir(root).join("settings.json"))
            .unwrap()
            .contains("v1:")
    );
}

#[tokio::test]
async fn secrets_are_refused_with_no_key_store_or_when_the_programs_environment_sets_them() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW, true, &["GH_TOKEN"], None);
    let (settings, app, cookie, csrf) = signed_in_app(root).await;
    // No key store given: fail closed.
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "chat-agent",
        json!({"secrets": {"ANTHROPIC_API_KEY": "k"}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    settings.set_program_keys(Some(Arc::new(MemStore::default())));
    let res = post_program(
        &app,
        &cookie,
        &csrf,
        "ci-runner",
        json!({"secrets": {"GH_TOKEN": TOKEN}}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let all = get_programs(&app, &cookie).await;
    assert_eq!(program(&all, "ci-runner")["secrets"][0]["source"], "env");
    assert!(!ci_dir(root).join("settings.json").exists());
}

#[tokio::test]
async fn secrets_are_saved_only_from_this_computer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_status(&ci_dir(root), program_status::CI_RUNNER, NOW, true, &[], None);
    let settings = settings_with_network(root, false, None);
    settings.set_program_keys(Some(Arc::new(MemStore::default())));
    let update = ProgramUpdate {
        settings: Default::default(),
        secrets: Some(json!({"GH_TOKEN": TOKEN})),
    };
    let refused = settings.save_program("ci-runner", &update, false).unwrap_err();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(!ci_dir(root).join("settings.json").exists());
    assert!(settings.save_program("ci-runner", &update, true).is_ok());
}

#[tokio::test]
async fn the_page_carries_the_programs_script_under_its_own_nonce_and_no_unfilled_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let (_, app, _, _) = signed_in_app(dir.path()).await;
    let res = app
        .oneshot(request("GET", "/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let csp = res.headers()["content-security-policy"].to_str().unwrap().to_string();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    let nonce = csp.split("'nonce-").nth(1).unwrap().split('\'').next().unwrap();
    assert!(html.contains("RT.programs = { init: init, load: load }"));
    assert!(html.contains("window.RT = { csrf: null }") && html.contains("--accent:"));
    assert!(!html.contains("{{"), "every placeholder is filled");
    assert_eq!(html.matches(&format!("<script nonce=\"{nonce}\">")).count(), 3);
    // Programs are shown as text, never as markup, and nothing in the script calls a program.
    let js = include_str!("../../assets/settings-programs.js");
    assert!(!js.contains("innerHTML") && !js.contains("insertAdjacentHTML"));
    assert!(!js.contains("fetch(") && js.matches("RT.api(").count() == 2);
}

#[test]
fn both_pages_have_the_program_tabs_and_the_panels_for_the_programs_script_to_fill() {
    // A tab shows for the page's own program and for each other one whose state is not `notInstalled` (the rows
    // `each_program_shows_its_state...` checks); the script's tab bar and panel host are in both pages, once each.
    for (name, page) in [
        ("settings.html", include_str!("../../assets/settings.html")),
        ("settings-ci.html", include_str!("../../assets/settings-ci.html")),
    ] {
        assert_eq!(page.matches("id=\"progTabs\"").count(), 1, "{name}");
        assert_eq!(page.matches("id=\"programView\"").count(), 1, "{name}");
        assert!(page.contains("RT.programs.init("), "{name}");
        assert!(
            !page.contains("programsList") && !page.contains("programCards"),
            "{name}"
        );
    }
    // The code agent's own sections move under a tab of their own, hidden while another program's is open.
    assert!(include_str!("../../assets/settings.html").contains("id=\"codeView\""));
    let js = include_str!("../../assets/settings-programs.js");
    assert!(
        js.contains("p.state !== \"notInstalled\""),
        "a program not on this computer gets no tab"
    );
    assert!(js.contains("localStorage"), "the selected tab is remembered per viewer");
}

#[test]
fn both_compose_files_share_one_data_volume_and_both_containers_run_as_uid_1000() {
    // docs/specs/docker.md section 3.1: the agent's page manages the CI runner through one volume, so what either
    // makes at 0600 must be readable to the other: the same uid, the socket's group added for the runner only.
    let agent = include_str!("../../docker/agent/compose.yaml");
    let ci = include_str!("../../docker/ci/compose.yaml");
    for (name, file) in [("agent", agent), ("ci", ci)] {
        assert!(
            file.contains("  data:\n    # ") && file.contains("    name: retaskable-data\n"),
            "{name}"
        );
        assert!(file.contains("      - data:/data\n"), "{name}");
    }
    assert!(ci.contains("user: \"1000:1000\"") && ci.contains("group_add:") && ci.contains("DOCKER_GID:?"));
    assert!(
        ci.contains("/var/run/docker.sock") && !agent.contains("docker.sock"),
        "only the runner has the socket"
    );
    let dockerfile = include_str!("../../docker/ci/Dockerfile");
    assert!(dockerfile.contains("--uid 1000") && dockerfile.contains("USER ci"));
    assert!(dockerfile.contains("RETASKABLE_CI_CREDENTIALS_FILE=/data/ci/credentials.json"));
    assert!(dockerfile.contains("RETASKABLE_SECRETS_KEY_FILE=/data/secrets.key"));
    assert!(include_str!("../../docker/agent/Dockerfile").contains("chown -R node:node /work /data"));
}
