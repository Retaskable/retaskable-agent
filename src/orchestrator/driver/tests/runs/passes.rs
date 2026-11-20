//! The passes that aren't about an item, in `runs` mode: the chat's asks and the session count they share with runs.
use super::super::asks::{Host, code_question, finishes, ops_named, push, store_failing};
use super::*;
use crate::orchestrator::driver::run_starters::RunStarters;

/// A `runs`-mode rig over the asks' fake store, with a runner for builds and `host` answering asks.
async fn runs_rig() -> (Rig, Arc<Host>, super::super::Asked, super::super::asks::Listed) {
    let (base, asked, listed) = store_failing("").await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).mode = OrchestratorMode::Runs;
    rig.lp(0).asks.poll_every = Duration::ZERO;
    rig.machine.run_starters = RunStarters(vec![Arc::new(FakeRuns::new(&[RunKind::Build]))]);
    let host = Arc::new(Host::default());
    rig.machine.asks = Some(host.clone());
    (rig, host, asked, listed)
}

async fn passes_until(rig: &mut Rig, asked: &super::super::Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.machine.run_passes().await;
        if finishes(asked).len() >= n {
            return finishes(asked);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the store never got {n} finishes");
}

#[tokio::test]
async fn a_runs_mode_project_answers_a_code_question() {
    let (mut rig, host, asked, listed) = runs_rig().await;
    push(&listed, code_question("in-1", "why is step 5 stuck?"));
    let sent = passes_until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["id"], "in-1");
    assert_eq!(sent[0]["result"]["answer"], "Answer to why is step 5 stuck?");
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
    assert!(ops_named(&asked, "intent_list").contains(&json!({})));
}

#[tokio::test]
async fn a_runs_mode_project_with_no_runner_asks_for_nothing_and_answers_nothing() {
    let (mut rig, host, asked, listed) = runs_rig().await;
    rig.machine.run_starters = RunStarters::default();
    push(&listed, code_question("in-1", "hello"));
    for _ in 0..5 {
        rig.machine.run_passes().await;
    }
    assert!(host.jobs.lock().unwrap().is_empty());
    assert!(ops_named(&asked, "intent_list").is_empty());
}

#[tokio::test]
async fn a_paused_runs_mode_project_holds_its_questions_until_resumed() {
    let (mut rig, host, asked, listed) = runs_rig().await;
    rig.pause("p0", true);
    push(&listed, code_question("in-1", "hello"));
    for _ in 0..5 {
        rig.machine.run_passes().await;
    }
    assert!(host.jobs.lock().unwrap().is_empty(), "the pause holds the question");
    rig.pause("p0", false);
    passes_until(&mut rig, &asked, 1).await;
}

#[tokio::test]
async fn runs_that_hold_a_session_count_toward_the_sessions_and_a_watch_or_merge_does_not() {
    let (mut rig, script) = setup(2).await;
    let starter = FakeRuns::new(&[RunKind::Build, RunKind::Plan, RunKind::Watch]);
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build"), run("p1", "plan"), run("w1", "watch")]);
    assert_eq!(rig.machine.sessions(), 0);
    pass(&mut rig, &starter, Instant::now()).await;
    assert_eq!(starter.started(), vec!["b1", "p1", "w1"]);
    assert_eq!(rig.machine.sessions(), 2, "the build and the plan, not the watch");
    starter.finish("b1", RunOutcome::PrOpened);
    pass(&mut rig, &starter, Instant::now()).await;
    assert_eq!(rig.machine.sessions(), 1);
}

#[tokio::test]
async fn acts_is_claim_with_a_starter_or_runs_with_a_runner_and_claims_is_the_first_only() {
    let (mut rig, _host, _asked, _listed) = runs_rig().await;
    let (runs, claims) = (
        |r: &Rig| r.machine.acts(&r.machine.projects[0]),
        |r: &Rig| r.machine.claims(&r.machine.projects[0]),
    );
    assert!(
        runs(&rig) && !claims(&rig),
        "runs with a runner acts but claims nothing"
    );
    rig.machine.run_starters = RunStarters::default();
    assert!(!runs(&rig), "runs with no runner does nothing");
    rig.go_live();
    assert!(runs(&rig) && claims(&rig), "claim with a starter does both");
}
