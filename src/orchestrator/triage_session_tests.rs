//! The triager on the engine chosen for it: a stub `cursor-agent` runs with the `top` model, a missing binary names
//! itself, and Claude is still the default.
#![cfg(unix)]
use std::sync::Arc;

use super::session::ClaudeStarter;
use crate::config::{CursorModels, Engine, EngineChoiceConfig, ModelsConfig, RoleEngines};
use crate::env::Env;
use crate::orchestrator::driver::{OutcomeSender, SessionResult, TriageEnd, TriageRequest};
use crate::orchestrator::session::tests::project_in;
use crate::triage::TriageContext;

fn context() -> TriageContext {
    TriageContext {
        reason: "stuck".into(),
        failed_in: String::new(),
        timeline: vec![],
        last_check_result: None,
        pr_url: None,
        branch: None,
        has_pushed_branch: false,
        has_session_to_continue: false,
        automatic_decisions: 0,
        transcripts_dir: None,
        known_bugs: vec![],
    }
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

struct Rig {
    dir: tempfile::TempDir,
    starter: Arc<ClaudeStarter>,
}

fn rig(choice: EngineChoiceConfig, with_cursor: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let cursor = root.join("cursor-agent");
    if with_cursor {
        let script = format!(
            "#!/bin/sh\necho \"$@\" > '{}'\nexit 3\n",
            root.join("args.txt").display()
        );
        crate::test_support::write_executable(&cursor, &script);
    }
    let path = cursor.to_string_lossy().into_owned();
    let env = Env::new(move |k| (k == "RETASKABLE_CURSOR").then(|| path.clone()));
    let starter = ClaudeStarter::new(
        root.join("data"),
        vec![project_in(&root, &repo)],
        choice,
        env,
        50,
        ModelsConfig::default(),
        CursorModels {
            top: Some("gpt-top".into()),
            implementer: Some("gpt-impl".into()),
            scout: Some("gpt-scout".into()),
        },
        150_000,
    );
    Rig { dir, starter }
}

fn request() -> (
    TriageRequest,
    tokio::sync::mpsc::UnboundedReceiver<crate::orchestrator::driver::SessionOutcome>,
) {
    let (outcomes, rx) = OutcomeSender::channel();
    let request = TriageRequest {
        project: "p".into(),
        item_id: "i".into(),
        context: context(),
        lane: None,
        usage: None,
        outcomes,
        placement: true,
    };
    (request, rx)
}

#[tokio::test]
async fn a_triage_with_chat_on_cursor_runs_cursor_agent_with_the_top_model() {
    let r = rig(chat_on(Engine::Cursor), true);
    let (req, mut rx) = request();
    r.starter.start_triage(req).unwrap();
    let got = rx.recv().await.unwrap();
    // The stub exits 3, so the look failed; what matters is that it ran.
    assert!(
        matches!(got.result, SessionResult::Triage(TriageEnd::Failed { .. })),
        "{got:?}"
    );
    let args = std::fs::read_to_string(r.dir.path().join("args.txt")).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("gpt-top"), "{args}");
}

#[tokio::test]
async fn a_missing_binary_names_itself() {
    let r = rig(chat_on(Engine::Cursor), false);
    let (req, _rx) = request();
    let err = r.starter.start_triage(req).unwrap_err();
    assert!(err.starts_with("the cursor-agent binary wasn't found ("), "{err}");
}
