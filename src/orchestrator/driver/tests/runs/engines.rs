//! Engine limits and sign-outs in `runs` mode: which engine a run is placed on, and when no run is asked for.
use super::*;

#[tokio::test]
async fn a_build_started_while_cursor_is_limited_is_placed_on_claude() {
    use crate::engines::EngineId::Claude;
    let starter = cursor_limited_build().await;
    let started = starter.started.lock().unwrap();
    assert_eq!(started[0].engine, Some(Claude));
    assert!(
        started[0]
            .context
            .fall
            .as_ref()
            .unwrap()
            .out
            .contains(&crate::engines::EngineId::Cursor)
    );
}

#[tokio::test]
async fn with_every_engine_out_no_run_take_is_sent() {
    use crate::engines::EngineId::{Claude, Codex, Cursor, Pi};
    let (mut rig, script) = setup(1).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["cursor"] }));
    start_rules_pass(
        &mut rig,
        &FakeRuns::new(&[RunKind::Build]),
        &[Claude, Cursor, Pi, Codex],
        false,
    )
    .await;
    assert!(script.lock().unwrap().args("run_take").is_empty());
}

#[tokio::test]
async fn a_build_takes_a_cursor_lane_when_claude_is_signed_out_and_claude_lanes_first_otherwise() {
    let lane = |rig: &Rig, s: &FakeRuns| {
        let want = crate::lanes::lane_path(std::path::Path::new(&rig.machine.projects[0].project.worktrees_dir), 1);
        let got = s.started.lock().unwrap()[0].lane.clone().unwrap();
        got == want
    };
    let (mut rig, script) = setup(2).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["claude", "cursor"] }));
    assert!(rig.machine.pool.claim_run("other", "x").is_some(), "lane 1 is busy");
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    let starter = FakeRuns::new(&[RunKind::Build]);
    start_rules_pass(&mut rig, &starter, &[], true).await;
    assert_eq!(starter.started(), vec!["b1"], "the Cursor lane takes it");
    assert!(!lane(&rig, &starter));

    let (mut rig, script) = setup(2).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["claude", "cursor"] }));
    script.lock().unwrap().takes = VecDeque::from([run("b1", "build")]);
    let starter = FakeRuns::new(&[RunKind::Build]);
    start_rules_pass(&mut rig, &starter, &[], false).await;
    assert!(lane(&rig, &starter), "Claude's lane first");
}

/// Claude at its usage limit holds a plan or build only without the model or
/// sign-in; with it they are asked for like any run (they start on the local model).
#[tokio::test]
async fn at_the_limit_runs_are_asked_for_with_the_fallback_and_held_without_it() {
    use crate::engines::EngineId::Claude;
    use crate::local_model::LocalModel;
    for (model, signed_out, asks) in [
        (LocalModel::up(), false, true),
        (LocalModel::off(), false, false),
        (LocalModel::up(), true, false),
    ] {
        let (mut rig, script) = setup(1).await;
        both_engines(&mut rig, json!({ "default": "claude" }));
        rig.machine.local = model;
        let starter = FakeRuns::new(&[RunKind::Build, RunKind::Plan]);
        start_rules_pass(&mut rig, &starter, &[Claude], signed_out).await;
        let takes = script.lock().unwrap().args("run_take");
        let case = signed_out;
        assert_eq!(takes.is_empty(), !asks, "{case:?}");
        if asks {
            assert_eq!(takes[0]["kinds"], json!(["build", "plan"]));
            assert!(
                takes[0]["agent"]["engines"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("claude"))
            );
        }
    }
}

/// `run_take` after one `run_passes` on a `runs` project that has the two engines.
async fn engines_asked_for(rig: &mut Rig, script: &Shared_) -> Vec<Value> {
    rig.wake(); // an empty answer holds the next ask until a wake
    rig.machine.run_passes().await;
    let takes = script.lock().unwrap().args("run_take");
    script.lock().unwrap().log.clear();
    takes
        .last()
        .map(|t| t["agent"]["engines"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_limit_the_machine_holds_keeps_the_next_run_pass_off_that_engine_until_it_clears() {
    use crate::engines::EngineId::Claude;
    let (mut rig, script) = runs_machine(Some(Arc::new(FakeRuns::new(&[RunKind::Build])))).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["cursor"] }));
    assert!(engines_asked_for(&mut rig, &script).await.contains(&json!("claude")));
    // A limit hit (as `settle` records it) is on the machine's own set, so the next pass sees it.
    let lifts = Duration::from_millis(300);
    rig.machine.engine_limits.hit(Claude, Instant::now() + lifts);
    assert!(!engines_asked_for(&mut rig, &script).await.contains(&json!("claude")));
    assert!(!engines_asked_for(&mut rig, &script).await.contains(&json!("claude")));
    // As time passes it clears.
    tokio::time::sleep(lifts).await;
    assert!(engines_asked_for(&mut rig, &script).await.contains(&json!("claude")));
}

#[tokio::test]
async fn a_signed_out_claude_the_machine_holds_keeps_run_passes_off_claude_until_it_clears() {
    let (mut rig, script) = runs_machine(Some(Arc::new(FakeRuns::new(&[RunKind::Build])))).await;
    both_engines(&mut rig, json!({ "default": "claude", "lanes": ["cursor"] }));
    rig.machine.claude_signed_out_until = Some(Instant::now() + Duration::from_secs(600));
    assert!(!engines_asked_for(&mut rig, &script).await.contains(&json!("claude")));
    rig.machine.claude_signed_out_until = Some(Instant::now());
    assert!(engines_asked_for(&mut rig, &script).await.contains(&json!("claude")));
}
