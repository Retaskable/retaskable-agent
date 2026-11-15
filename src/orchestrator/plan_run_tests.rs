use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::config::{Engine, EngineChoiceConfig};
use crate::orchestrator::driver::{RunEndSender, RunProcesses};
use crate::orchestrator::fake_session_tests::{
    assert_ran_on, assert_ran_read_only_in, cut_registered_tree, on_every_engine, roster, rows_once_running,
};
use crate::orchestrator::look_run::LookRuns;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    starter: PlanRuns,
}

fn rig(out: &str, extra: &str, choice: EngineChoiceConfig) -> Rig {
    let (dir, root, roster) = roster(out, extra, choice, |p| p.budget.planner_usd = 1.5);
    Rig {
        _dir: dir,
        root,
        starter: PlanRuns::new(roster, Slot::default()),
    }
}

fn answer(structured: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{structured},"session_id":"s"}}"#
    )
}

const GOOD: &str = r#"{"pieces":[{"title":"A","what":"w","doneWhen":"d","needs":[]},{"title":"B","what":"w","doneWhen":"d","needs":[1]}]}"#;

struct Started {
    ends: UnboundedReceiver<RunEnd>,
    trees: UnboundedReceiver<(String, Arc<ProcessTree>)>,
    stop: crate::runs::RunStop,
}

fn request(piece: Value) -> (RunRequest, Started) {
    let (ends, rx) = RunEndSender::channel();
    let (processes, trees) = RunProcesses::channel();
    let stop = crate::runs::RunStop::default();
    let request = RunRequest {
        token: "tok".into(),
        project: "p".into(),
        processes,
        stop: stop.clone(),
        kind: RunKind::Plan,
        piece: serde_json::from_value(piece).unwrap(),
        other: Default::default(),
        lane: None,
        engine: None,
        ends,
        events: crate::orchestrator::driver::RunEventSender::channel().0,
        context: crate::orchestrator::driver::RunContext::for_test(),
    };
    (request, Started { ends: rx, trees, stop })
}

fn brief() -> Value {
    json!({ "id": "it1", "title": "Add the widget", "specs": ["docs/specs/widget.md"] })
}

async fn end_of(rx: &mut UnboundedReceiver<RunEnd>) -> RunEnd {
    tokio::time::timeout(Duration::from_secs(60), rx.recv())
        .await
        .expect("an end")
        .expect("the channel stays open")
}

#[tokio::test]
async fn a_good_plan_ends_planned_in_the_base_checkout_read_only() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let base = lanes::base_checkout_path(&PathBuf::from(&r.starter.roster.projects[0].worktrees_dir));
    std::fs::create_dir_all(&base).unwrap();
    let (req, mut s) = request(brief());
    assert_eq!(r.starter.kinds(), vec![RunKind::Plan]);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Planned, "{:?}", end.result);
    assert_eq!(end.result["pieces"][1]["needs"], json!(["A"]));
    assert_ran_read_only_in(&r.root, &base, "opus");
    let prompt = std::fs::read_to_string(r.root.join("in.txt")).unwrap();
    assert!(prompt.contains("Add the widget"), "{prompt}");
    // The flag clears once the run has ended.
    tokio::time::timeout(Duration::from_secs(5), async {
        while r.starter.kinds().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_plan_is_listed_while_it_runs_and_not_after() {
    let r = rig(&answer(GOOD), "sleep 1", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    let rows = rows_once_running(&r.starter.roster.sessions).await;
    assert_eq!(rows.len(), 1);
    let info = &rows[0].info;
    assert_eq!(
        (info.project.as_str(), info.item_id.as_str(), info.role),
        ("p", "it1", Role::Planner)
    );
    assert_eq!((info.title.as_deref(), info.lane), (Some("Add the widget"), None));
    assert_eq!((info.engine, info.model.as_str()), (EngineId::Claude, tiers::OPUS));
    end_of(&mut s.ends).await;
    assert!(r.starter.roster.sessions.snapshot().is_empty());
}

#[tokio::test]
async fn a_plan_that_fails_to_start_leaves_no_row() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let (mut req, _s) = request(brief());
    req.project = "nope".into();
    assert!(r.starter.start(req).is_err());
    assert!(r.starter.roster.sessions.snapshot().is_empty());
}

#[tokio::test]
async fn without_a_base_checkout_it_runs_in_the_repo() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::Planned);
    let pwd = std::fs::read_to_string(r.root.join("pwd.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(r.root.join("repo")).unwrap()
    );
}

#[tokio::test]
async fn a_bad_plan_ends_failed_with_why() {
    let r = rig(&answer(r#"{"pieces":[]}"#), "", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Failed("The plan has no pieces.".into()));
}

#[tokio::test]
async fn a_session_that_errors_ends_failed() {
    let r = rig("", "exit 3", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert!(matches!(end_of(&mut s.ends).await.outcome, RunOutcome::Failed(_)));
}

#[tokio::test]
async fn an_unfillable_prompt_ends_failed_outside() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let mut piece = brief();
    piece["what"] = json!("use {{thing}}");
    let (req, mut s) = request(piece);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(matches!(end.outcome, RunOutcome::Failed(_)));
    assert_eq!(end.result["outside"], true);
}

#[tokio::test]
async fn one_plan_at_a_time_and_none_for_another_engine() {
    let r = rig(&answer(GOOD), "sleep 60", EngineChoiceConfig::default());
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert!(r.starter.kinds().is_empty());
    let (second, _s2) = request(brief());
    assert!(r.starter.start(second).unwrap_err().contains("already running"));
    // Cutting the registered tree ends the run with nothing from the task.
    cut_registered_tree(&mut s.trees).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while r.starter.kinds().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(s.ends.try_recv().is_err());
    assert!(!s.stop.stopped());
}

#[tokio::test]
async fn the_planner_role_on_another_engine_runs_that_engines_binary() {
    for (engine, program) in [(Engine::Cursor, "cursor"), (Engine::Codex, "codex"), (Engine::Pi, "pi")] {
        let mut choice = EngineChoiceConfig::default();
        choice.roles.planner = Some(engine);
        let mut r = rig(&answer(GOOD), "", choice);
        let root = r.root.clone();
        on_every_engine(&mut r.starter.roster, &root);
        assert_eq!(r.starter.kinds(), vec![RunKind::Plan]);
        let (req, mut s) = request(brief());
        r.starter.start(req).unwrap();
        // The stub answers in Claude's format, so only where the session ran is judged, not how it ended.
        end_of(&mut s.ends).await;
        assert_ran_on(&root, program);
    }
}

#[tokio::test]
async fn a_plan_told_to_start_on_claude_while_the_planner_is_cursor_runs_on_claude() {
    let mut choice = EngineChoiceConfig::default();
    choice.roles.planner = Some(Engine::Cursor);
    let mut r = rig(&answer(GOOD), "", choice);
    let root = r.root.clone();
    on_every_engine(&mut r.starter.roster, &root);
    let (mut req, mut s) = request(brief());
    req.engine = Some(EngineId::Claude);
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::Planned);
    let args = std::fs::read_to_string(root.join("args.txt")).unwrap();
    assert!(
        args.contains("--permission-mode") && !args.contains("gpt-top"),
        "{args}"
    );
}

#[tokio::test]
async fn a_missing_binary_of_the_chosen_engine_gives_the_run_back_naming_it() {
    let mut choice = EngineChoiceConfig::default();
    choice.roles.planner = Some(Engine::Cursor);
    let mut r = rig(&answer(GOOD), "", choice);
    let gone = r.root.join("nothing-here").to_string_lossy().into_owned();
    r.starter.roster.env = Env::new(move |k| (k == "RETASKABLE_CURSOR").then(|| gone.clone()));
    let (req, _s) = request(brief());
    assert!(
        r.starter
            .start(req)
            .unwrap_err()
            .contains("cursor-agent binary wasn't found")
    );
    assert_eq!(r.starter.kinds(), vec![RunKind::Plan]);
}

#[tokio::test]
async fn a_plan_and_a_look_share_one_slot() {
    let r = rig(&answer(GOOD), "sleep 60", EngineChoiceConfig::default());
    let look = LookRuns::new(r.starter.roster.clone(), r.starter.slot.clone());
    // A look holds the slot: no plan is asked for or started.
    let held = r.starter.slot.take().unwrap();
    assert!(r.starter.kinds().is_empty() && look.kinds().is_empty());
    let (req, _s) = request(brief());
    assert!(r.starter.start(req).unwrap_err().contains("already running"));
    drop(held);
    // A plan holds it: no look is asked for or started.
    let (req, mut s) = request(brief());
    r.starter.start(req).unwrap();
    assert!(look.kinds().is_empty());
    let (mut req, _s2) = request(brief());
    req.kind = RunKind::Look;
    assert!(look.start(req).unwrap_err().contains("already running"));
    cut_registered_tree(&mut s.trees).await;
}

/// What `begin` offered the tier closure as "local may be used" for a rig with the local model as given.
fn offered_local(up: bool, engine: Engine) -> bool {
    let mut choice = EngineChoiceConfig::default();
    choice.roles.planner = Some(engine);
    let mut r = rig(&answer(GOOD), "", choice);
    let root = r.root.clone();
    on_every_engine(&mut r.starter.roster, &root);
    if up {
        r.starter.roster.local = crate::local_model::LocalModel::up();
    }
    let (req, _s) = request(brief());
    let mut offered = None;
    r.starter
        .roster
        .begin(Role::Planner, &req, &Slot::default(), |local| {
            offered = Some(local);
            tiers::OPUS
        })
        .unwrap();
    offered.unwrap()
}

#[test]
fn the_tier_is_offered_the_local_model_only_with_the_model_up_on_claude() {
    assert!(offered_local(true, Engine::Claude));
    assert!(!offered_local(false, Engine::Claude), "local down");
    assert!(!offered_local(true, Engine::Cursor), "not on Claude");
}

#[test]
fn the_planner_is_on_the_top_tier_and_local_only_at_the_limit_with_the_model_up() {
    assert_eq!(tiers::floor(tiers::OPUS, false, true), tiers::OPUS);
    assert_eq!(tiers::floor(tiers::OPUS, true, true), tiers::LOCAL);
    assert_eq!(tiers::floor(tiers::OPUS, true, false), tiers::OPUS);
}
