//! The implementer session runs on the engine its request names, with that engine's binary, and a fresh start that
//! moves to another engine runs on that one (engines/03 sections 3.1 and 3.3). Stub binaries log what they were asked.
#![cfg(unix)]
use std::time::Duration;

use super::tests::{fake_gh, project_in};
use super::*;
use crate::backlog::ItemStatus;
use crate::config::LaneEngine;
use crate::orchestrator::driver::OutcomeSender;

const FINISHED: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened","summary":"done"},"session_id":"s"}"#;
const COMMIT_BIG: &str = "seq 1 1100 | sed 's/^/let v/;s/$/ = 1;/' > big.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm big";

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    starter: Arc<ClaudeStarter>,
}

/// Stubs for all four engines, each logging `<program> <args>` to `calls.txt` and answering `pr_opened`. The
/// `claude` stub first runs `claude_step` in the lane. Programs in `missing` get no stub.
fn rig(choice: EngineChoiceConfig, claude_step: &str, missing: &[&str]) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = crate::test_support::repo_with_origin(&root);
    let mut vars = crate::test_support::stub_engine_vars(&root, FINISHED, claude_step, missing);
    let gh = root.join("gh.sh");
    fake_gh(&gh, &root);
    vars.push(("RETASKABLE_GH", gh.to_string_lossy().into_owned()));
    let env = Env::new(move |k| vars.iter().find(|(v, _)| *v == k).map(|(_, p)| p.clone()));
    let cursor_models = crate::config::CursorModels {
        implementer: Some("gpt-5".into()),
        ..Default::default()
    };
    let starter = ClaudeStarter::new(
        root.join("data"),
        vec![project_in(&root, &repo)],
        choice,
        env,
        50,
        150_000,
        cursor_models,
        Default::default(),
        crate::local_model::LocalModel::off(),
    );
    Rig {
        _dir: dir,
        root,
        starter,
    }
}

fn request(engine: EngineId, resume: bool) -> (StartRequest, tokio::sync::mpsc::UnboundedReceiver<SessionOutcome>) {
    let (outcomes, rx) = OutcomeSender::channel();
    let item = Item {
        merge_commit: None,
        id: "implementer-session".into(),
        title: "Have a session write the code".into(),
        status: ItemStatus::Building,
        rank: 1,
        do_next: false,
        depends_on: Vec::new(),
        spec_refs: Vec::new(),
        slice: 2,
        remaining: Some("Wire it.".into()),
        pr_url: None,
        branch: None,
        kind: None,
        milestone: None,
        draft_id: None,
        reopened: None,
        no_fix_sha: None,
        size: None,
        tier: None,
        red_sha: None,
    };
    let request = StartRequest {
        project: "p".into(),
        item_id: item.id.clone(),
        lane: 1,
        token: "t".into(),
        item: item.clone(),
        session_id: "sess-1".into(),
        engine,
        resume,
        outcomes,
        usage: None,
        follow_up: None,
        items: vec![item],
        attempt: 0,
        notes: None,
        takeover: None,
    };
    (request, rx)
}

async fn run_on(rig: &Rig, engine: EngineId, resume: bool) -> SessionOutcome {
    let (request, mut rx) = request(engine, resume);
    if resume {
        // A resumed session needs its lane left as it was.
        let lane = lanes::lane_path(Path::new(&rig.starter.projects[0].worktrees_dir), 1);
        std::fs::create_dir_all(&lane).unwrap();
    }
    rig.starter.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .expect("an outcome")
        .expect("the channel stays open")
}

fn calls(rig: &Rig) -> Vec<String> {
    let log = std::fs::read_to_string(rig.root.join("calls.txt")).unwrap_or_default();
    log.lines().map(str::to_string).collect()
}

/// The first line of each transcript in the run folder.
fn meta_lines(rig: &Rig) -> Vec<String> {
    let dir = paths::run_dir(&rig.root.join("data"), "p", "implementer-session");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(e.unwrap().path()).unwrap();
        out.push(text.lines().next().unwrap_or("").to_string());
    }
    out
}

#[tokio::test]
async fn a_cursor_session_runs_cursor_agent_with_the_implementer_model() {
    let rig = rig(EngineChoiceConfig::default(), "", &[]);
    run_on(&rig, EngineId::Cursor, false).await;
    let calls = calls(&rig);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].starts_with("cursor-agent "), "{calls:?}");
    assert!(calls[0].contains("--model gpt-5"), "{calls:?}");
    let metas = meta_lines(&rig);
    assert!(metas[0].contains(r#""engine":"cursor""#), "{metas:?}");
}

#[tokio::test]
async fn codex_and_pi_sessions_run_their_own_binaries() {
    for (engine, program) in [(EngineId::Codex, "codex"), (EngineId::Pi, "pi")] {
        let rig = rig(EngineChoiceConfig::default(), "", &[]);
        run_on(&rig, engine, false).await;
        let calls = calls(&rig);
        assert_eq!(calls.len(), 1, "{program}: {calls:?}");
        assert!(calls[0].starts_with(&format!("{program} ")), "{calls:?}");
        assert!(meta_lines(&rig)[0].contains(&format!(r#""engine":"{program}""#)));
    }
}

#[tokio::test]
async fn a_resume_stays_on_the_engine_it_started_on() {
    let choice = EngineChoiceConfig {
        lanes: vec![LaneEngine::Cursor],
        ..Default::default()
    };
    let rig = rig(choice, "", &[]);
    run_on(&rig, EngineId::Claude, true).await;
    let calls = calls(&rig);
    assert!(calls.len() == 1 && calls[0].starts_with("claude "), "{calls:?}");
    assert!(calls[0].contains("--resume sess-1"), "{calls:?}");
}

#[tokio::test]
async fn a_fresh_start_for_the_lane_engine_runs_on_the_new_engine() {
    // The first run commits an oversized file, so the gates send findings to a fresh session, which the lane's
    // own engine (Cursor) now takes.
    let choice = EngineChoiceConfig {
        lanes: vec![LaneEngine::Cursor],
        ..Default::default()
    };
    let rig = rig(choice, COMMIT_BIG, &[]);
    run_on(&rig, EngineId::Claude, false).await;
    let calls = calls(&rig);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].starts_with("claude "), "{calls:?}");
    assert!(calls[1].starts_with("cursor-agent "), "{calls:?}");
    assert!(calls[1].contains("--model gpt-5"), "{calls:?}");
    let metas = meta_lines(&rig);
    assert!(metas.iter().any(|m| m.contains(r#""engine":"cursor""#)), "{metas:?}");
}

#[tokio::test]
async fn a_fresh_start_for_an_engine_limited_takeover_runs_on_the_takeover() {
    let rig = rig(EngineChoiceConfig::default(), COMMIT_BIG, &[]);
    let (mut request, mut rx) = request(EngineId::Claude, false);
    request.takeover = Some(EngineId::Codex);
    rig.starter.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let calls = calls(&rig);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[1].starts_with("codex "), "{calls:?}");
}

#[tokio::test]
async fn a_missing_binary_hands_the_claim_back_without_starting() {
    let rig = rig(EngineChoiceConfig::default(), "", &["cursor-agent"]);
    let (request, _rx) = request(EngineId::Cursor, false);
    let err = rig.starter.start(request).unwrap_err();
    assert!(err.contains("cursor-agent binary wasn't found"), "{err}");
    assert_eq!(rig.starter.active(), 0);
}

#[tokio::test]
async fn a_start_over_job_starts_claude_on_opus() {
    let rig = rig(EngineChoiceConfig::default(), "", &[]);
    let (mut request, mut rx) = request(EngineId::Claude, false);
    request.attempt = 1;
    request.item.tier = Some("opus".into());
    rig.starter.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let calls = calls(&rig);
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("claude ") && c.contains("--model opus")),
        "{calls:?}"
    );
}
