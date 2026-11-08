use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::config::{Engine, EngineChoiceConfig};
use crate::engines::Role;
use crate::env::Env;
use crate::orchestrator::driver::{RunEndSender, RunProcesses};
use crate::orchestrator::fake_session_tests::{assert_ran_read_only_in, cut_registered_tree, roster};
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
        ends,
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
    assert_ran_read_only_in(&r.root, &base);
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

    let mut other = EngineChoiceConfig::default();
    other.roles.planner = Some(Engine::Codex);
    assert!(rig(&answer(GOOD), "", other).starter.kinds().is_empty());
}

/// `r` placed on `engine` for `role` with `engine-placement` On: every engine's binary is the fake, and the
/// Cursor `top` model is `gpt-5`.
fn placed_on(roster: &mut Roster, root: &std::path::Path, engine: Engine, role: Role) {
    let bin = root
        .join(crate::orchestrator::session::tests::fake_name())
        .to_string_lossy()
        .into_owned();
    roster.env = Env::new(move |k| {
        matches!(
            k,
            "RETASKABLE_CLAUDE" | "RETASKABLE_CURSOR" | "RETASKABLE_CODEX" | "RETASKABLE_PI"
        )
        .then(|| bin.clone())
    });
    roster.flags = Arc::new(crate::flags::Flags::fixed(&["engine-placement"]));
    roster.cursor_models.top = Some("gpt-5".into());
    match role {
        Role::Planner => roster.choice.roles.planner = Some(engine),
        _ => roster.choice.roles.chat = Some(engine),
    }
}

#[tokio::test]
async fn with_placement_on_a_session_runs_on_the_chosen_engine() {
    for engine in [Engine::Cursor, Engine::Codex, Engine::Pi] {
        let mut r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
        placed_on(&mut r.starter.roster, &r.root, engine, Role::Planner);
        assert_eq!(r.starter.kinds(), vec![RunKind::Plan], "{engine:?}");
        let (req, mut s) = request(brief());
        r.starter.start(req).unwrap();
        end_of(&mut s.ends).await;
        let args = std::fs::read_to_string(r.root.join("args.txt")).unwrap();
        assert!(!args.contains("--max-budget-usd"), "{engine:?}: {args}");
        if engine == Engine::Cursor {
            assert!(args.contains("--model gpt-5"), "{args}");
        }
    }
}

#[tokio::test]
async fn with_placement_on_a_missing_binary_gives_the_run_back_naming_it() {
    let mut r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    placed_on(&mut r.starter.roster, &r.root, Engine::Codex, Role::Planner);
    let gone = r.root.join("nothing-here").to_string_lossy().into_owned();
    r.starter.roster.env = Env::new(move |k| (k == "RETASKABLE_CODEX").then(|| gone.clone()));
    let (req, _s) = request(brief());
    let err = r.starter.start(req).unwrap_err();
    assert!(err.contains("codex binary wasn't found"), "{err}");
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
