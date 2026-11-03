use super::*;
use crate::config::{Engine, EngineChoiceConfig};
use crate::env::Env;
use crate::lanes;
use crate::orchestrator::starter_fixtures::{self as fixtures, Started, answer, end_of};

type Rig = fixtures::Rig<LookRuns>;

fn rig(out: &str, extra: &str, choice: EngineChoiceConfig) -> Rig {
    fixtures::rig(out, extra, choice, |p| p.budget.reviewer_usd = 1.5, LookRuns::new)
}

const GOOD: &str = r#"{"decision":"again","reason":"it was close","instructions":"Mind the tests."}"#;

fn request(piece: Value, other: Value) -> (RunRequest, Started) {
    fixtures::request(RunKind::Look, piece, other)
}

fn brief() -> Value {
    json!({ "id": "pc1", "title": "Show the thing", "what": "Show it.", "doneWhen": "It shows.",
        "attempts": [{ "kind": "build", "outcome": "failed", "why": "tests red" }] })
}

async fn until_free(r: &Rig) {
    fixtures::until(|| !r.starter.kinds().is_empty()).await;
}

#[tokio::test]
async fn a_decision_ends_looked_with_its_wire_json_in_the_base_checkout() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let base = lanes::base_checkout_path(&PathBuf::from(&r.starter.setup.projects[0].worktrees_dir));
    std::fs::create_dir_all(&base).unwrap();
    let (req, mut s) = request(
        brief(),
        json!({ "knownBugs": [{ "id": "b1", "title": "A known bug" }] }),
    );
    assert_eq!(r.starter.kinds(), vec![RunKind::Look]);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.token, "tok");
    assert_eq!(end.outcome, RunOutcome::Looked, "{:?}", end.result);
    assert_eq!(
        end.result,
        json!({ "decision": "again", "reason": "it was close", "instructions": "Mind the tests." })
    );
    fixtures::assert_read_only_in(&r.root, &base, "1.5");
    let prompt = std::fs::read_to_string(r.root.join("in.txt")).unwrap();
    assert!(
        prompt.contains("Show the thing") && prompt.contains("tests red") && prompt.contains("A known bug"),
        "{prompt}"
    );
    // The transcript goes under the run's folder, keyed by the piece.
    let dir = paths::run_dir(&r.starter.setup.data_dir, "p", "pc1");
    assert!(
        dir.read_dir()
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().starts_with("look-"))
    );
    until_free(&r).await;
}

#[tokio::test]
async fn without_a_base_checkout_it_runs_in_the_repo() {
    let r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    let (req, mut s) = request(brief(), json!({}));
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::Looked);
    let pwd = std::fs::read_to_string(r.root.join("pwd.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(r.root.join("repo")).unwrap()
    );
}

#[tokio::test]
async fn a_failed_session_or_an_unusable_answer_ends_failed_with_the_note() {
    let r = rig("", "exit 3", EngineChoiceConfig::default());
    let (req, mut s) = request(brief(), json!({}));
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(
        matches!(&end.outcome, RunOutcome::Failed(n) if n.starts_with("The look session")),
        "{end:?}"
    );

    // `again` without instructions can't be carried out.
    let r = rig(
        &answer(r#"{"decision":"again","reason":"r"}"#),
        "",
        EngineChoiceConfig::default(),
    );
    let (req, mut s) = request(brief(), json!({}));
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(
        matches!(&end.outcome, RunOutcome::Failed(n) if n.contains("no instructions")),
        "{end:?}"
    );

    let r = rig(&answer("null"), "", EngineChoiceConfig::default());
    let (req, mut s) = request(brief(), json!({}));
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(
        matches!(&end.outcome, RunOutcome::Failed(n) if n.starts_with("The look session")),
        "{end:?}"
    );
}

#[tokio::test]
async fn another_engine_or_a_missing_binary_gives_the_run_back() {
    let mut other = EngineChoiceConfig::default();
    other.roles.chat = Some(Engine::Codex);
    let r = rig(&answer(GOOD), "", other);
    assert!(r.starter.kinds().is_empty());
    let (req, _s) = request(brief(), json!({}));
    assert!(r.starter.start(req).unwrap_err().contains("only Claude"));

    let mut r = rig(&answer(GOOD), "", EngineChoiceConfig::default());
    r.starter.setup.env = Env::new(|k| (k == "RETASKABLE_CLAUDE").then(|| "/nowhere/claude".to_string()));
    let (req, _s) = request(brief(), json!({}));
    assert!(r.starter.start(req).is_err());
    assert!(r.starter.slot.is_free());
}

#[tokio::test]
async fn one_look_at_a_time_a_cut_run_sends_no_end_and_the_slot_frees() {
    let r = rig(&answer(GOOD), "sleep 60", EngineChoiceConfig::default());
    let (req, mut s) = request(brief(), json!({}));
    r.starter.start(req).unwrap();
    assert!(r.starter.kinds().is_empty());
    let (second, _s2) = request(brief(), json!({}));
    assert!(r.starter.start(second).unwrap_err().contains("already running"));
    // The session's tree is registered with the run; cutting it ends the run with nothing sent.
    fixtures::cut_registered_tree(&mut s).await;
    until_free(&r).await;
    assert_eq!(r.starter.kinds(), vec![RunKind::Look]);
    assert!(s.ends.try_recv().is_err());
}

#[tokio::test]
async fn a_run_stopped_by_the_loop_sends_no_second_end() {
    let r = rig(&answer(GOOD), "sleep 1", EngineChoiceConfig::default());
    let (req, mut s) = request(brief(), json!({}));
    s.stop.set();
    r.starter.start(req).unwrap();
    until_free(&r).await;
    assert!(s.ends.try_recv().is_err());
}
