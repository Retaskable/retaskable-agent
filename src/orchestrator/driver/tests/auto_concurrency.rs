//! Auto concurrency in the loop (`driver/auto.rs`, docs/specs/agent/07.1-model-tiers.md): the pacer's number caps
//! the sessions the machine starts, behind the `auto-concurrency` flag.
use super::*;
use crate::config::AutoConcurrency;
use crate::engines::EngineId;
use crate::store::RunRecord;

const T: i64 = 1_000_000;
const RESET: i64 = T + 4 * 3600;

fn reading(u: f64, resets: i64) -> Value {
    json!({"unifiedWindows": {"five_hour": {"utilization": u, "resetsAt": resets}}})
}

/// A rig with 8 lanes, 12 queued items, `concurrency` 3 and the flag `on`; the clock stands at `T`.
async fn rig_with(on: bool, cfg: AutoConcurrency) -> (Rig, Arc<Fake>) {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(8, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if on { &["auto-concurrency"] } else { &[] }));
    let fake = rig.go_live();
    rig.machine.concurrency = 3;
    rig.machine.auto = crate::orchestrator::driver::auto::Auto::new(cfg);
    let ids: Vec<String> = (0..12).map(|i| format!("q{i:02}")).collect();
    rig.queue(0, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    rig.machine.clock = Some(T);
    (rig, fake)
}

fn started(fake: &Fake) -> usize {
    fake.started.lock().unwrap().len()
}

/// Moves the clock to `T + secs`, gives the pacer a reading, and runs a pass with `running` sessions going.
async fn step(rig: &mut Rig, fake: &Fake, secs: i64, u: f64, resets: i64, running: u32) {
    rig.machine.clock = Some(T + secs);
    rig.lp(0).pace_readings.push(reading(u, resets));
    fake.active.store(running, Ordering::SeqCst);
    rig.machine.poll().await;
}

fn max_of(max: u32) -> AutoConcurrency {
    AutoConcurrency {
        max,
        ..Default::default()
    }
}

#[tokio::test]
async fn behind_pace_with_work_queued_starts_one_more_after_the_step_and_never_more_than_max() {
    let (mut rig, fake) = rig_with(true, max_of(4)).await;
    step(&mut rig, &fake, 0, 0.10, RESET, 0).await;
    assert_eq!(started(&fake), 3, "starts at concurrency");
    step(&mut rig, &fake, 60, 0.1001, RESET, 3).await;
    assert_eq!(started(&fake), 3, "no step yet: 5 minutes haven't passed");
    step(&mut rig, &fake, 300, 0.1002, RESET, 3).await;
    assert_eq!(started(&fake), 4, "one more after the step");
    for k in 2..6 {
        step(&mut rig, &fake, 300 * k, 0.1002 + 0.0001 * k as f64, RESET, 4).await;
    }
    assert_eq!(started(&fake), 4, "never more than max");
    assert_eq!(rig.machine.effective_concurrency(), 4);
}

#[tokio::test]
async fn ahead_of_pace_lowers_the_starts_down_to_min_and_stops_nothing() {
    let (mut rig, fake) = rig_with(
        true,
        AutoConcurrency {
            min: 2,
            ..Default::default()
        },
    )
    .await;
    step(&mut rig, &fake, 0, 0.10, RESET, 0).await;
    assert_eq!(started(&fake), 3);
    step(&mut rig, &fake, 300, 0.30, RESET, 3).await;
    assert_eq!(rig.machine.effective_concurrency(), 2);
    step(&mut rig, &fake, 600, 0.50, RESET, 3).await;
    assert_eq!(rig.machine.effective_concurrency(), 2, "not below min");
    assert_eq!(started(&fake), 3, "nothing new starts while 3 run, and none is stopped");
    step(&mut rig, &fake, 610, 0.50, RESET, 1).await;
    assert_eq!(started(&fake), 4, "with 1 running, the 2 allowed leave room for one");
}

#[tokio::test]
async fn a_new_window_sets_the_number_back_to_concurrency_at_once() {
    let (mut rig, fake) = rig_with(true, AutoConcurrency::default()).await;
    step(&mut rig, &fake, 0, 0.10, RESET, 0).await;
    step(&mut rig, &fake, 300, 0.30, RESET, 3).await;
    step(&mut rig, &fake, 600, 0.50, RESET, 3).await;
    assert_eq!(rig.machine.effective_concurrency(), 1);
    step(&mut rig, &fake, 610, 0.02, RESET + 5 * 3600, 3).await;
    assert_eq!(rig.machine.effective_concurrency(), 3, "no wait for the 5 minutes");
}

#[tokio::test]
async fn with_the_flag_off_or_the_setting_off_starts_are_capped_at_concurrency() {
    for (on, enabled) in [(false, true), (true, false)] {
        let cfg = AutoConcurrency { enabled, ..max_of(8) };
        let (mut rig, fake) = rig_with(on, cfg).await;
        for k in 0..4 {
            step(
                &mut rig,
                &fake,
                300 * k,
                0.10 + 0.0001 * k as f64,
                RESET,
                started(&fake) as u32,
            )
            .await;
        }
        assert_eq!(started(&fake), 3, "flag {on}, enabled {enabled}");
        assert_eq!(rig.machine.effective_concurrency(), 3);
    }
}

#[tokio::test]
async fn a_cursor_sessions_reading_does_not_reach_the_pacer() {
    for (engine, taken) in [(EngineId::Cursor, 0), (EngineId::Claude, 1)] {
        let (mut rig, _fake) = rig_with(true, AutoConcurrency::default()).await;
        at(&mut rig, 0, "a", ItemState::Implementing, Some(1));
        rig.lp(0).records.insert(
            "a".into(),
            RunRecord {
                impl_session_engine: engine,
                ..Default::default()
            },
        );
        rig.machine.outcome_sender().send(SessionOutcome {
            project: "p0".into(),
            item_id: "a".into(),
            lane: 1,
            result: SessionResult::NeedsHuman("x".into()),
            usage: Some(reading(0.4, RESET)),
            context_tokens: None,
            notes: vec![],
            session_id: None,
        });
        rig.machine.settle_outcomes().await;
        assert_eq!(rig.lp(0).pace_readings.len(), taken, "{engine:?}");
    }
}

/// Restarts the machine: the same pacer settings and `dir`, nothing else carried.
fn restart(rig: &mut Rig, dir: &std::path::Path) {
    rig.machine.auto = crate::orchestrator::driver::auto::Auto::new(AutoConcurrency::default());
    rig.machine.auto.dir = Some(dir.to_path_buf());
}

#[tokio::test]
async fn a_restart_in_the_same_window_keeps_the_number_and_after_the_reset_starts_from_concurrency() {
    let (mut rig, fake) = rig_with(true, AutoConcurrency::default()).await;
    let dir = tempfile::tempdir().unwrap();
    restart(&mut rig, dir.path());
    step(&mut rig, &fake, 0, 0.10, RESET, 0).await;
    step(&mut rig, &fake, 300, 0.30, RESET, 3).await;
    step(&mut rig, &fake, 600, 0.50, RESET, 3).await;
    assert_eq!(rig.machine.effective_concurrency(), 1);

    restart(&mut rig, dir.path());
    rig.machine.clock = Some(T + 700);
    rig.machine.poll().await;
    assert_eq!(rig.machine.effective_concurrency(), 1, "the number carries over");
    assert!(!rig.machine.auto.pacer.due(T + 700), "and so does the 5-minute clock");

    restart(&mut rig, dir.path());
    rig.machine.clock = Some(RESET + 10);
    rig.machine.poll().await;
    assert_eq!(
        rig.machine.effective_concurrency(),
        3,
        "a reset window starts from concurrency"
    );
}

#[tokio::test]
async fn with_the_flag_off_nothing_is_read_or_written() {
    let (mut rig, fake) = rig_with(false, AutoConcurrency::default()).await;
    let dir = tempfile::tempdir().unwrap();
    restart(&mut rig, dir.path());
    step(&mut rig, &fake, 0, 0.10, RESET, 0).await;
    step(&mut rig, &fake, 300, 0.50, RESET, 3).await;
    assert!(!dir.path().join("machine.json").exists());
}
