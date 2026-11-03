use std::time::Duration;

use super::*;
use crate::lanes;
use crate::orchestrator::starter_fixtures::{self as fixtures, Started, answer, end_of, fixture};

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    starter: PlanRuns,
}

fn rig(out: &str, extra: &str, choice: EngineChoiceConfig) -> Rig {
    let f = fixture(out, extra);
    let mut project = f.project;
    project.budget.planner_usd = 1.5;
    let starter = PlanRuns::new(
        f.root.join("data"),
        vec![project],
        choice,
        f.env,
        ModelsConfig::default(),
    );
    Rig {
        _dir: f._dir,
        root: f.root,
        starter,
    }
}

const GOOD: &str = r#"{"pieces":[{"title":"A","what":"w","doneWhen":"d","needs":[]},{"title":"B","what":"w","doneWhen":"d","needs":[1]}]}"#;

fn request(piece: Value) -> (RunRequest, Started) {
    fixtures::request(RunKind::Plan, piece, json!({}))
}

fn brief() -> Value {
    json!({ "id": "it1", "title": "Add the widget", "specs": ["docs/specs/widget.md"] })
}

#[tokio::test]
async fn a_good_plan_ends_planned_in_the_base_checkout_read_only() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let base = lanes::base_checkout_path(&PathBuf::from(&r.starter.projects[0].worktrees_dir));
    std::fs::create_dir_all(&base).unwrap();
    let (req, mut s) = request(brief());
    assert_eq!(r.starter.kinds(), vec![RunKind::Plan]);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::Planned, "{:?}", end.result);
    assert_eq!(end.result["pieces"][1]["needs"], json!(["A"]));
    let pwd = std::fs::read_to_string(r.root.join("pwd.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(&base).unwrap()
    );
    let args = std::fs::read_to_string(r.root.join("args.txt")).unwrap();
    assert!(
        args.contains("--max-budget-usd 1.5") && args.contains("sonnet"),
        "{args}"
    );
    assert!(
        !args.contains("--dangerously") && args.contains("--disallowedTools") && args.contains("Edit Write"),
        "{args}"
    );
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
    let (token, tree) = tokio::time::timeout(Duration::from_secs(30), s.trees.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token, "tok");
    tree.cut();
    tree.kill_and_wait(Duration::from_secs(5));
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
