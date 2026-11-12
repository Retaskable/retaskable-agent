use super::*;
use crate::backlog::BacklogError;
use crate::lane_pool::PoolRules;

fn pool() -> LanePool {
    LanePool::new(PoolRules {
        lanes: 2,
        ..PoolRules::default()
    })
}

fn refused() -> Result<(), RunDoneError> {
    Err(RunDoneError::Other(BacklogError::Unreachable("down".into())))
}

fn hold(pool: &LanePool, lane: u32) -> bool {
    pool.holder(lane).is_some()
}

#[test]
fn a_build_takes_a_lane_and_a_watch_takes_none() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "b", RunKind::Build, "piece-1", t0).unwrap();
    runs.start(&mut pool, "w", RunKind::Watch, "piece-2", t0).unwrap();
    assert_eq!(runs.live()[0].lane, Some(1));
    assert_eq!(runs.live()[1].lane, None);
    assert!(hold(&pool, 1) && !hold(&pool, 2));
}

#[test]
fn a_build_with_no_free_lane_does_not_start() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "a", RunKind::Fix, "x", t0).unwrap();
    runs.start(&mut pool, "b", RunKind::Build, "y", t0).unwrap();
    assert_eq!(runs.start(&mut pool, "c", RunKind::Build, "z", t0), Err(NoLane));
    assert_eq!(runs.live().len(), 2);
}

#[test]
fn the_beat_lists_every_live_run_but_a_merge_turn_which_beats_on_its_own() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "a", RunKind::Build, "x", t0).unwrap();
    runs.start(&mut pool, "b", RunKind::Merge, "y", t0).unwrap();
    runs.start(&mut pool, "c", RunKind::Look, "z", t0).unwrap();
    assert_eq!(runs.beat_tokens(), ["a", "c"]);
    assert_eq!(runs.merge_tokens(), ["b"]);
    assert!(runs.merging());
}

fn sleeper() -> (tokio::process::Child, Arc<ProcessTree>) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("ping");
        c.args(["-n", "60", "127.0.0.1"]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sleep");
        c.arg("60");
        c
    };
    ProcessTree::prepare(&mut cmd);
    let child = cmd.spawn().unwrap();
    let tree = ProcessTree::join(&child).unwrap();
    (child, tree)
}

#[tokio::test]
async fn a_stop_cuts_the_tree_and_ends_the_run_interrupted_with_its_lane_freed() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "a", RunKind::Build, "x", t0).unwrap();
    runs.start(&mut pool, "b", RunKind::Build, "y", t0).unwrap();
    let (mut child, tree) = sleeper();
    runs.add_process("a", tree.clone());
    let verdicts = HashMap::from([("a".to_string(), BeatVerdict::Stop), ("b".to_string(), BeatVerdict::Ok)]);
    runs.apply_beat(&mut pool, &verdicts, t0);
    assert!(tree.was_cut());
    assert!(child.wait().await.is_ok(), "the child is gone");
    assert_eq!(runs.beat_tokens(), ["b"]);
    assert!(!hold(&pool, 1) && hold(&pool, 2));
    let ends = runs.due_ends(t0);
    assert_eq!(
        (ends[0].token.as_str(), &ends[0].outcome),
        ("a", &RunOutcome::Interrupted)
    );
}

#[test]
fn a_build_past_its_ceiling_less_the_margin_fails_and_a_watch_of_the_same_age_does_not() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "b", RunKind::Build, "x", t0).unwrap();
    runs.start(&mut pool, "w", RunKind::Watch, "y", t0).unwrap();
    let before = t0 + Duration::from_secs(3 * 3600) - CEILING_MARGIN - Duration::from_secs(1);
    assert!(runs.end_past_ceilings(&mut pool, before).is_empty());
    let at = t0 + Duration::from_secs(3 * 3600) - CEILING_MARGIN;
    assert_eq!(runs.end_past_ceilings(&mut pool, at), ["b"]);
    assert_eq!(runs.beat_tokens(), ["w"]);
    assert!(!hold(&pool, 1));
    match &runs.due_ends(at)[0].outcome {
        RunOutcome::Failed(why) => assert!(why.contains("3-hour ceiling"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn ceilings_by_kind() {
    let h = |k| ceiling(k).as_secs() / 3600;
    assert_eq!([h(RunKind::Build), h(RunKind::Fix), h(RunKind::Watch)], [3, 3, 6]);
    assert_eq!([h(RunKind::Plan), h(RunKind::Look), h(RunKind::Merge)], [1, 1, 1]);
}

#[test]
fn a_stale_end_is_dropped_and_another_refusal_is_due_again_after_a_minute() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "a", RunKind::Watch, "x", t0).unwrap();
    runs.start(&mut pool, "b", RunKind::Watch, "y", t0).unwrap();
    runs.end(&mut pool, "a", RunOutcome::Green, json!({}), t0);
    runs.end(&mut pool, "b", RunOutcome::Green, json!({}), t0);
    assert_eq!(runs.due_ends(t0).len(), 2);
    runs.end_sent("a", Err(RunDoneError::Stale), t0);
    runs.end_sent("b", refused(), t0);
    assert!(runs.due_ends(t0).is_empty());
    assert!(runs.due_ends(t0 + END_RETRY - Duration::from_secs(1)).is_empty());
    let later = runs.due_ends(t0 + END_RETRY);
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].token, "b");
    runs.end_sent("b", Ok(()), t0 + END_RETRY);
    assert!(runs.due_ends(t0 + END_RETRY * 9).is_empty());
}

#[test]
fn a_lane_goes_back_on_every_outcome() {
    let outcomes = [
        RunOutcome::Interrupted,
        RunOutcome::Failed("x".into()),
        RunOutcome::PrOpened,
        RunOutcome::Questions,
        RunOutcome::PersonSteps,
        RunOutcome::NothingLeft,
    ];
    for outcome in outcomes {
        let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
        runs.start(&mut pool, "a", RunKind::Build, "x", t0).unwrap();
        assert!(hold(&pool, 1));
        assert!(runs.end(&mut pool, "a", outcome.clone(), json!({}), t0));
        assert!(!hold(&pool, 1), "{outcome:?}");
        assert!(!runs.end(&mut pool, "a", outcome, json!({}), t0), "ended once");
    }
}

fn verdicts(pairs: &[(&str, BeatVerdict)]) -> HashMap<String, BeatVerdict> {
    pairs.iter().map(|(t, v)| (t.to_string(), *v)).collect()
}

#[test]
fn lock_tokens_beat_alongside_run_tokens() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "w", RunKind::Watch, "x", t0).unwrap();
    runs.locks().add("L1", "chore-tidy", t0);
    assert_eq!(runs.beat_tokens(), ["w", "L1"]);
}

#[test]
fn a_stop_for_a_lock_marks_only_that_lock_lost() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    let locks = runs.locks();
    locks.add("L1", "merge-main", t0);
    locks.add("L2", "chore-tidy", t0);
    runs.start(&mut pool, "w", RunKind::Watch, "x", t0).unwrap();
    runs.apply_beat(
        &mut pool,
        &verdicts(&[("L1", BeatVerdict::Stop), ("L2", BeatVerdict::Ok)]),
        t0,
    );
    assert!(!locks.held("L1") && locks.held("L2"));
    assert_eq!(runs.live().len(), 1);
    assert_eq!(runs.beat_tokens(), ["w", "L2"]);
}

#[test]
fn a_lock_past_its_ceiling_margin_is_lost() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    let locks = runs.locks();
    locks.add("M", "merge-main", t0);
    locks.add("C", "chore-tidy", t0);
    runs.end_past_ceilings(
        &mut pool,
        t0 + Duration::from_secs(3600) - CEILING_MARGIN - Duration::from_secs(1),
    );
    assert!(locks.held("M"));
    runs.end_past_ceilings(&mut pool, t0 + Duration::from_secs(3600) - CEILING_MARGIN);
    assert!(!locks.held("M") && locks.held("C"));
    runs.end_past_ceilings(&mut pool, t0 + Duration::from_secs(6 * 3600) - CEILING_MARGIN);
    assert!(!locks.held("C"));
}

#[test]
fn held_locks_count_in_held_until_dropped() {
    let (mut runs, t0) = (Runs::new("p"), Instant::now());
    let locks = runs.locks();
    assert_eq!(runs.held(), 0);
    locks.add("L1", "chore-tidy", t0);
    assert_eq!(runs.held(), 1);
    locks.remove("L1");
    assert_eq!(runs.held(), 0);
    let _ = &mut runs;
}

#[test]
fn a_finished_tree_is_dropped_from_the_run_when_the_next_is_added() {
    let (mut pool, mut runs, t0) = (pool(), Runs::new("p"), Instant::now());
    runs.start(&mut pool, "w", RunKind::Watch, "x", t0).unwrap();
    for _ in 0..5 {
        // The worker lets go of its tree once its process is done; only the run still holds it.
        let mut cmd = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            cmd.args(["/C", "exit"]);
        }
        ProcessTree::prepare_sync(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let tree = Arc::new(ProcessTree::join_sync(&child).unwrap());
        child.wait().unwrap();
        runs.add_process("w", tree);
    }
    assert_eq!(runs.live()[0].processes.len(), 1);
}
