//! The assistant and the triager run on the engine chosen for chat (engines/03 sections 3.1 and 3.4): stub
//! binaries log what they were asked, and a missing binary gives the apology naming it.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

use crate::backlog::Client;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::env::Env;
use crate::orchestrator::ask_host::ClaudeAsks;
use crate::orchestrator::driver::{
    AskHost, AskJob, AskStore, OutcomeSender, SessionResult, SessionStarter, TriageEnd, TriageRequest,
};
use crate::orchestrator::session::ClaudeStarter;
use crate::orchestrator::session::tests::project_in;
use crate::triage::TriageContext;
use crate::view::AskCommand;

const ANSWER: &str = r#"{"answer":"Step 5 is waiting.","actions":[]}"#;
const DECIDED: &str = r#"{"decision":"skip","reason":"Not needed."}"#;

/// What each program's stream says when its session answers with `structured`.
fn stream(program: &str, structured: &str) -> String {
    let fenced = format!("```json\n{structured}\n```");
    match program {
        "cursor-agent" => {
            json!({"type":"result","subtype":"success","is_error":false,"result":fenced,"session_id":"s"}).to_string()
        }
        "codex" => format!(
            "{}\n{}",
            json!({"type":"item.completed","item":{"type":"agent_message","text":fenced}}),
            json!({"type":"turn.completed","usage":{}})
        ),
        "pi" => format!(
            "{}\n{}",
            json!({"type":"message_end","message":{"role":"assistant","content":fenced}}),
            json!({"type":"agent_settled"})
        ),
        _ => format!(
            r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured},"session_id":"s"}}"#
        ),
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    env: Env,
    project: ProjectConfig,
    choice: EngineChoiceConfig,
    models: CursorModels,
}

/// Stubs for the four engines answering `out`, each logging `<program> <args>` to `calls.txt`; programs in
/// `missing` get none. Chat runs on `engine`.
fn rig(engine: Engine, out: &str, missing: &[&str]) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = crate::test_support::repo_with_origin(&root);
    let vars = crate::test_support::engine_stubs(&root, missing, |_| String::new(), |p| stream(p, out));
    let mut choice = EngineChoiceConfig::default();
    choice.roles.chat = Some(engine);
    Rig {
        project: project_in(&root, &repo),
        _dir: dir,
        env: Env::new(move |k| vars.iter().find(|(v, _)| *v == k).map(|(_, p)| p.clone())),
        root,
        choice,
        models: CursorModels {
            top: Some("opus-top".into()),
            implementer: None,
            scout: Some("fast-scout".into()),
        },
    }
}

fn calls(rig: &Rig) -> String {
    std::fs::read_to_string(rig.root.join("calls.txt")).unwrap_or_default()
}

async fn answer(rig: &Rig, placement: bool) -> String {
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(|Json(body): Json<Value>| async move {
            let value = match (body["op"].as_str().unwrap_or(""), body["args"]["kind"].as_str()) {
                ("asks_list", _) => json!({ "asks": [] }),
                ("facts_get", Some("audiences")) => json!({ "audiences": [] }),
                ("facts_get", _) => json!({}),
                _ => json!([]),
            };
            Json(json!({ "value": value }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let host = ClaudeAsks::new(
        rig.root.join("data"),
        rig.choice.clone(),
        rig.env.clone(),
        ModelsConfig::default(),
        rig.models.clone(),
        0.0,
        Arc::new(crate::ask::perform::tests::view("p", rig.root.clone(), "")),
    );
    let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
    let job = AskJob {
        project: rig.project.clone(),
        ask: AskCommand {
            project: "p".into(),
            ask_id: "ask-1".into(),
            text: "why is step 5 stuck?".into(),
            reply_to: None,
            about: None,
            images_block: String::new(),
        },
        store: AskStore {
            client: Arc::new(Client::new(format!("http://{addr}"))),
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
async fn an_ask_with_chat_on_cursor_runs_cursor_with_the_scout_model() {
    let rig = rig(Engine::Cursor, ANSWER, &[]);
    assert_eq!(answer(&rig, true).await, "Step 5 is waiting.");
    let log = calls(&rig);
    assert!(
        log.starts_with("cursor-agent ") && log.contains("--model fast-scout"),
        "{log}"
    );
}

#[tokio::test]
async fn an_ask_on_codex_and_pi_runs_that_engine() {
    for (engine, program) in [(Engine::Codex, "codex"), (Engine::Pi, "pi")] {
        let rig = rig(engine, ANSWER, &[]);
        assert_eq!(answer(&rig, true).await, "Step 5 is waiting.");
        assert!(calls(&rig).starts_with(program), "{program}");
    }
}

#[tokio::test]
async fn an_ask_with_placement_off_stays_on_claude() {
    let rig = rig(Engine::Cursor, ANSWER, &[]);
    assert_eq!(answer(&rig, false).await, "Step 5 is waiting.");
    assert!(calls(&rig).starts_with("claude "), "{}", calls(&rig));
}

#[tokio::test]
async fn an_ask_with_its_binary_missing_apologises_naming_it() {
    let rig = rig(Engine::Cursor, ANSWER, &["cursor-agent"]);
    let said = answer(&rig, true).await;
    assert!(
        said.starts_with("Sorry, I couldn't answer that: the cursor-agent binary wasn't found ("),
        "{said}"
    );
}

fn starter(rig: &Rig) -> Arc<ClaudeStarter> {
    ClaudeStarter::new(
        rig.root.join("data"),
        vec![rig.project.clone()],
        rig.choice.clone(),
        rig.env.clone(),
        50,
        ModelsConfig::default(),
        150_000,
        rig.models.clone(),
    )
}

fn triage_request(
    placement: bool,
) -> (
    TriageRequest,
    tokio::sync::mpsc::UnboundedReceiver<crate::orchestrator::driver::SessionOutcome>,
) {
    let (outcomes, rx) = OutcomeSender::channel();
    let context = TriageContext {
        reason: "stuck".into(),
        failed_in: "reviewing".into(),
        timeline: vec![],
        last_check_result: None,
        pr_url: None,
        branch: None,
        has_pushed_branch: false,
        has_session_to_continue: false,
        automatic_decisions: 0,
        transcripts_dir: None,
        known_bugs: vec![],
    };
    let request = TriageRequest {
        project: "p".into(),
        item_id: "item".into(),
        context,
        lane: None,
        usage: None,
        outcomes,
        placement,
    };
    (request, rx)
}

#[tokio::test]
async fn a_triage_with_chat_on_cursor_runs_cursor_with_the_top_model() {
    let rig = rig(Engine::Cursor, DECIDED, &[]);
    let (request, mut rx) = triage_request(true);
    // `triage` is the trait's entry; it spawns the session and sends its end.
    SessionStarter::triage(starter(&rig).as_ref(), request).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(outcome.result, SessionResult::Triage(TriageEnd::Found { .. })),
        "{:?}",
        outcome.result
    );
    let log = calls(&rig);
    assert!(
        log.starts_with("cursor-agent ") && log.contains("--model opus-top"),
        "{log}"
    );
}

#[tokio::test]
async fn a_triage_with_placement_off_or_a_missing_binary_is_as_before() {
    let rig = rig(Engine::Cursor, DECIDED, &["cursor-agent"]);
    let (request, _rx) = triage_request(true);
    let why = SessionStarter::triage(starter(&rig).as_ref(), request).unwrap_err();
    assert!(why.starts_with("the cursor-agent binary wasn't found ("), "{why}");
    let (request, mut rx) = triage_request(false);
    SessionStarter::triage(starter(&rig).as_ref(), request).unwrap();
    tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(calls(&rig).starts_with("claude "), "{}", calls(&rig));
    let _: &Path = &rig.root;
}
