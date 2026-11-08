//! The assistant and the triager run on the engine chat is placed on, with that engine's binary and, for Cursor, the
//! role's model (engines/03 sections 3.1 and 3.4). Stub binaries log what they were asked.
#![cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::ClaudeStarter;
use super::tests::project_in;
use crate::backlog::Client;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig, RoleEngines};
use crate::env::Env;
use crate::orchestrator::ask_host::ClaudeAsks;
use crate::orchestrator::driver::{
    AskHost, AskJob, AskStore, OutcomeSender, SessionOutcome, SessionResult, TriageEnd, TriageRequest,
};
use crate::view::{AgentView, AskCommand};

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

async fn ask(root: &std::path::Path, choice: EngineChoiceConfig, env: Env, placement: bool) -> String {
    let base = empty_store().await;
    let repo = crate::test_support::repo_with_origin(root);
    let view = Arc::new(AgentView::blank());
    let host = ClaudeAsks::new(
        root.join("data"),
        choice,
        env,
        ModelsConfig::default(),
        models(),
        0.0,
        view,
    );
    let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
    let job = AskJob {
        project: project_in(root, &repo),
        ask: AskCommand {
            project: "p".into(),
            ask_id: "a1".into(),
            text: "How is it going?".into(),
            reply_to: None,
            about: None,
            images_block: String::new(),
        },
        store: AskStore {
            client: Arc::new(Client::new(base)),
            repo: 1,
            token: "t".into(),
        },
        usage: None,
        lanes: vec![],
        runs: Default::default(),
        events,
        placement,
    };
    host.run(job).await.unwrap().answer
}

#[tokio::test]
async fn an_ask_on_cursor_runs_cursor_with_the_scout_model_and_returns_its_answer() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), &cursor_says(r#"{"answer":"All good.","actions":[]}"#), &[]);
    let answer = ask(dir.path(), chat_on(Engine::Cursor), env, true).await;
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
        let answer = ask(dir.path(), chat_on(engine), env, true).await;
        assert!(!answer.contains("only Claude"), "{answer}");
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
    let answer = ask(dir.path(), chat_on(Engine::Cursor), env, true).await;
    assert!(
        answer.starts_with("Sorry, I couldn't answer that: the cursor-agent binary wasn't found ("),
        "{answer}"
    );
}

fn starter(root: &std::path::Path, choice: EngineChoiceConfig, env: Env) -> (Arc<ClaudeStarter>, PathBuf) {
    let repo = crate::test_support::repo_with_origin(root);
    let starter = ClaudeStarter::new(
        root.join("data"),
        vec![project_in(root, &repo)],
        choice,
        env,
        50,
        ModelsConfig::default(),
        models(),
        150_000,
    );
    (starter, repo)
}

fn triage_request() -> (TriageRequest, tokio::sync::mpsc::UnboundedReceiver<SessionOutcome>) {
    let (outcomes, rx) = OutcomeSender::channel();
    let request = TriageRequest {
        project: "p".into(),
        item_id: "x".into(),
        context: crate::test_support::triage_context(vec![], "bug"),
        lane: None,
        usage: None,
        outcomes,
        placement: true,
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

#[tokio::test]
async fn with_placement_off_only_claude_answers() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), "", &[]);
    let answer = ask(dir.path(), chat_on(Engine::Cursor), env, false).await;
    assert!(answer.contains("only Claude sessions run"), "{answer}");
    assert!(calls(dir.path()).is_empty());
}

#[test]
fn a_triage_with_placement_off_is_refused_on_another_engine() {
    let dir = tempfile::tempdir().unwrap();
    let env = stubs(dir.path(), "", &[]);
    let (starter, _repo) = starter(dir.path(), chat_on(Engine::Cursor), env);
    let (mut request, _rx) = triage_request();
    request.placement = false;
    assert!(starter.start_triage(request).unwrap_err().contains("only Claude"));
}
