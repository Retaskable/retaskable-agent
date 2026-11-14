//! The assistant and the triager run on the engine chat is placed on, with that engine's binary and, for Cursor, the
//! role's model (engines/03 sections 3.1 and 3.4). Stub binaries log what they were asked.
#![cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::ClaudeStarter;
use super::tests::project_in;
use crate::backlog::Client;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, RoleEngines};
use crate::env::Env;
use crate::intents::Outcome;
use crate::orchestrator::ask_host::ClaudeAsks;
use crate::orchestrator::driver::{
    AskHost, AskJob, AskStore, OutcomeSender, SessionOutcome, SessionResult, TriageEnd, TriageRequest,
};
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

fn starter(root: &std::path::Path, choice: EngineChoiceConfig, env: Env) -> (Arc<ClaudeStarter>, PathBuf) {
    let repo = crate::test_support::repo_with_origin(root);
    let starter = ClaudeStarter::new(
        root.join("data"),
        vec![project_in(root, &repo)],
        choice,
        env,
        50,
        150_000,
        models(),
        Default::default(),
        crate::local_model::LocalModel::off(),
    );
    (starter, repo)
}

fn triage_request() -> (TriageRequest, tokio::sync::mpsc::UnboundedReceiver<SessionOutcome>) {
    triage_request_listed(Default::default())
}

fn triage_request_listed(sessions: Sessions) -> (TriageRequest, tokio::sync::mpsc::UnboundedReceiver<SessionOutcome>) {
    let (outcomes, rx) = OutcomeSender::channel();
    let request = TriageRequest {
        usage: None,
        engine: None,
        project: "p".into(),
        item_id: "x".into(),
        context: crate::test_support::triage_context(vec![], "bug"),
        lane: None,
        outcomes,
        sessions,
    };
    (request, rx)
}

#[tokio::test]
async fn a_triage_on_cursor_runs_cursor_with_the_top_model() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(
        dir.path(),
        &cursor_says(r#"{"decision":"continue","reason":"r","instructions":"go on"}"#),
        &[],
    );
    let (starter, _repo) = starter(dir.path(), chat_on(Engine::Cursor), env);
    let (request, mut rx) = triage_request();
    starter.start_triage(request).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(outcome.result, SessionResult::Triage(TriageEnd::Found { .. })),
        "{:?}",
        outcome.result
    );
    let calls = calls(dir.path());
    assert!(calls.len() == 1 && calls[0].starts_with("cursor-agent "), "{calls:?}");
    assert!(calls[0].contains("--model big-one"), "{calls:?}");
}

#[test]
fn a_triage_with_the_binary_missing_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), "", &["cursor-agent"]);
    let (starter, _repo) = starter(dir.path(), chat_on(Engine::Cursor), env);
    let (request, _rx) = triage_request();
    let err = starter.start_triage(request).unwrap_err();
    assert!(err.starts_with("the cursor-agent binary wasn't found ("), "{err}");
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

#[tokio::test]
async fn a_triage_is_listed_with_no_lane_while_it_runs_and_not_after() {
    let dir = tempfile::tempdir().unwrap();
    let (env, go) = held(dir.path());
    let (starter, _repo) = starter(dir.path(), EngineChoiceConfig::default(), env);
    let sessions = Sessions::default();
    let (request, mut rx) = triage_request_listed(sessions.clone());
    starter.start_triage(request).unwrap();
    let rows = sessions.rows_when_running().await;
    assert_eq!(rows.len(), 1);
    let info = &rows[0].info;
    assert_eq!((info.project.as_str(), info.item_id.as_str()), ("p", "x"));
    assert_eq!((info.role, info.lane), (crate::engines::Role::Triager, None));
    std::fs::write(go, "").unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(sessions.snapshot().is_empty());
}
