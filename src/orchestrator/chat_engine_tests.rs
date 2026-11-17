//! The assistant and the triager run on the engine chat is placed on, with that engine's binary and, for Cursor, the
//! role's model (engines/03 sections 3.1 and 3.4). Stub binaries log what they were asked.
#![cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

use super::tests::project_in;
use crate::backlog::Client;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, RoleEngines};
use crate::env::Env;
use crate::intents::Outcome;
use crate::jobs::host::{JobHost, JobRole, JobSession};
use crate::orchestrator::ask_host::ClaudeAsks;
use crate::orchestrator::driver::{AskHost, AskJob, AskStore};
use crate::orchestrator::job_host::ClaudeJobs;
use crate::orchestrator::live::sessions::Sessions;
use crate::view::AgentView;

/// A Cursor-style answer carrying `json` as the model's text.
fn cursor_says(json: &str) -> String {
    let text = format!("```json\n{json}\n```");
    serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":text,"session_id":"c"}).to_string()
}

fn chat_on(engine: Engine) -> EngineChoiceConfig {
    EngineChoiceConfig {
        roles: RoleEngines {
            chat: Some(engine),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn models() -> CursorModels {
    CursorModels {
        top: Some("big-one".into()),
        implementer: None,
        scout: Some("small-one".into()),
    }
}

fn stubs(root: &std::path::Path, out: &str, missing: &[&str]) -> Env {
    crate::test_support::stub_engines(root, out, "", missing)
}

fn calls(root: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(root.join("calls.txt"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// A store with nothing in it: lists for `list` and `drafts`, empty facts for the rest.
async fn empty_store() -> String {
    use axum::{Json, Router, routing::post};
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(|Json(body): Json<serde_json::Value>| async move {
            let list = matches!(body["op"].as_str(), Some("list" | "draft_list" | "drafts"));
            Json(serde_json::json!({ "value": if list { serde_json::json!([]) } else { serde_json::json!({}) } }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn ask(root: &std::path::Path, choice: EngineChoiceConfig, env: Env) -> String {
    ask_listed(root, choice, env, Default::default()).await
}

async fn ask_listed(root: &std::path::Path, choice: EngineChoiceConfig, env: Env, sessions: Sessions) -> String {
    let base = empty_store().await;
    let repo = crate::test_support::repo_with_origin(root);
    let view = Arc::new(AgentView::blank());
    let host = ClaudeAsks::new(
        root.join("data"),
        choice,
        env,
        models(),
        0.0,
        view,
        crate::local_model::LocalModel::off(),
    );
    let job = AskJob {
        engine: None,
        project: project_in(root, &repo),
        id: "a1".into(),
        question: "How is it going?".into(),
        images_block: String::new(),
        store: AskStore {
            client: Arc::new(Client::new(base)),
            repo: 1,
            project_id: "p".into(),
            token: "t".into(),
            failures: Default::default(),
        },
        lanes: vec![],
        runs: Default::default(),
        sessions,
    };
    match host.run(job).await {
        Outcome::Done(v) => v["answer"].as_str().unwrap_or_default().to_string(),
        Outcome::Failed(why) => why,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_ask_on_cursor_runs_cursor_with_the_scout_model_and_returns_its_answer() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), &cursor_says(r#"{"answer":"All good.","actions":[]}"#), &[]);
    let answer = ask(dir.path(), chat_on(Engine::Cursor), env).await;
    assert_eq!(answer, "All good.");
    let calls = calls(dir.path());
    assert!(calls.len() == 1 && calls[0].starts_with("cursor-agent "), "{calls:?}");
    assert!(calls[0].contains("--model small-one"), "{calls:?}");
}

#[tokio::test]
async fn an_ask_on_codex_or_pi_runs_that_engine_not_the_claude_apology() {
    for (engine, program) in [(Engine::Codex, "codex"), (Engine::Pi, "pi")] {
        let dir = tempfile::tempdir().unwrap();
        let env = stubs(dir.path(), "", &[]);
        let answer = ask(dir.path(), chat_on(engine), env).await;
        assert!(!answer.contains("Claude alone"), "{answer}");
        let calls = calls(dir.path());
        assert!(
            calls.len() == 1 && calls[0].starts_with(&format!("{program} ")),
            "{calls:?}"
        );
    }
}

#[tokio::test]
async fn an_ask_with_the_binary_missing_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), "", &["cursor-agent"]);
    let answer = ask(dir.path(), chat_on(Engine::Cursor), env).await;
    assert!(answer.starts_with("the cursor-agent binary wasn't found ("), "{answer}");
}

/// A job's session as the host places it on `choice`'s engines.
async fn job_session(
    root: &std::path::Path,
    choice: EngineChoiceConfig,
    env: Env,
    role: JobRole,
) -> Result<Box<dyn JobSession>, String> {
    let repo = crate::test_support::repo_with_origin(root);
    let host = ClaudeJobs::new(
        root.join("data"),
        choice,
        env,
        models(),
        crate::local_model::LocalModel::off(),
        Sessions::default(),
    );
    host.session(&project_in(root, &repo), role).await
}

#[tokio::test]
async fn a_triage_job_on_cursor_is_placed_on_cursor_with_the_reviewers_budget() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), "", &[]);
    let s = job_session(dir.path(), chat_on(Engine::Cursor), env, JobRole::TriageCheck)
        .await
        .unwrap();
    let c = s.config();
    assert_eq!(c.engine, Some(crate::engines::EngineId::Cursor));
    assert!(c.cursor.is_some());
    assert!(c.cwd.is_dir() && c.shell);
}

#[tokio::test]
async fn a_job_with_the_binary_missing_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), "", &["cursor-agent"]);
    let err = job_session(dir.path(), chat_on(Engine::Cursor), env, JobRole::TriageLook)
        .await
        .err()
        .unwrap();
    assert!(err.starts_with("the cursor-agent binary wasn't found ("), "{err}");
}

#[tokio::test]
async fn a_vet_or_release_job_runs_on_the_writers_engine_and_budgets() {
    let choice = EngineChoiceConfig {
        roles: RoleEngines {
            writer: Some(Engine::Codex),
            ..Default::default()
        },
        ..Default::default()
    };
    for role in [JobRole::Vet, JobRole::Release] {
        let dir = tempfile::tempdir().unwrap();
        let env = stubs(dir.path(), "", &[]);
        let s = job_session(dir.path(), choice.clone(), env.clone(), role)
            .await
            .unwrap();
        assert_eq!(s.config().engine, Some(crate::engines::EngineId::Codex));
    }
}

/// A claude that waits for `go` before it answers, so a test can look at what is listed meanwhile.
fn held(root: &std::path::Path) -> (Env, PathBuf) {
    let go = root.join("go");
    let step = format!("while [ ! -f '{}' ]; do sleep 0.05; done", go.display());
    (crate::test_support::stub_engines(root, "", &step, &[]), go)
}

#[tokio::test]
async fn an_ask_is_listed_with_no_lane_while_it_runs_and_not_after() {
    let dir = tempfile::tempdir().unwrap();
    let (env, go) = held(dir.path());
    let sessions = Sessions::default();
    let (root, listed) = (dir.path().to_path_buf(), sessions.clone());
    let task = tokio::spawn(async move { ask_listed(&root, EngineChoiceConfig::default(), env, listed).await });
    let rows = sessions.rows_when_running().await;
    assert_eq!(rows.len(), 1);
    let info = &rows[0].info;
    assert_eq!((info.project.as_str(), info.item_id.as_str()), ("p", "a1"));
    assert_eq!((info.role, info.lane), (crate::engines::Role::Assistant, None));
    assert_eq!(info.engine, crate::engines::EngineId::Claude);
    assert!(!info.model.is_empty());
    std::fs::write(go, "").unwrap();
    task.await.unwrap();
    assert!(sessions.snapshot().is_empty());
}
