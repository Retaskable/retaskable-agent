use std::collections::BTreeMap;

use super::*;
use crate::item_lifecycle::ItemRun;
use crate::lane_pool::{LanePool, PoolRules};
use crate::orchestrator::steps::Running;
use crate::telemetry::event;

const PROJECT: &str = "proj";

fn pool(lanes: u32) -> LanePool {
    LanePool::new(PoolRules {
        lanes,
        ..Default::default()
    })
}

fn make_input<'a>(
    runs: &'a BTreeMap<String, ItemRun>,
    leases: &'a BTreeMap<String, String>,
    records: &'a BTreeMap<String, RunRecord>,
    running: &'a Running,
    pool: &'a LanePool,
    actions: &'a [Action],
    recent: &'a [RecentEvent<'a>],
) -> Input<'a> {
    Input {
        project: PROJECT,
        paused: false,
        runs,
        leases,
        records,
        running,
        pool,
        actions,
        recent,
        now_ms: 0,
        restart: None,
        lease_idle_minutes: LEASE_IDLE_MINUTES,
        idle_lanes_minutes: IDLE_LANES_MINUTES,
        no_progress_minutes: NO_PROGRESS_MINUTES,
    }
}

fn leased_run(state: ItemState) -> ItemRun {
    let mut run = ItemRun::new();
    run.state = state;
    run
}

fn checking_record(lane: u32) -> RunRecord {
    RunRecord {
        state: "checking".into(),
        lane: Some(lane),
        impl_session_id: None,
        impl_session_engine: Default::default(),
        branch: None,
        impl_context_tokens: None,
        slice: 1,
        attempt: 0,
        ..Default::default()
    }
}

fn take_for_item(pool: &mut LanePool, lane: u32, item: &str) {
    pool.take(
        lane,
        Holder::Item {
            project: PROJECT.into(),
            item: item.into(),
        },
    );
}

#[test]
fn lease_idle_fires_only_for_needs_human() {
    let mut runs = BTreeMap::new();
    runs.insert("queued-item".to_string(), leased_run(ItemState::Queued));
    runs.insert("stuck-item".to_string(), leased_run(ItemState::NeedsHuman));
    let mut leases = BTreeMap::new();
    leases.insert("queued-item".to_string(), "t1".to_string());
    leases.insert("stuck-item".to_string(), "t2".to_string());
    let records = BTreeMap::new();
    let running = Running::default();
    let pool = pool(2);
    let actions = Vec::new();
    let recent = Vec::new();
    let input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);

    let found = check(&input);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "lease_idle:stuck-item");
    assert_eq!(found[0].rule, "lease_idle");
    assert_eq!(found[0].threshold_minutes, LEASE_IDLE_MINUTES);
}

#[test]
fn double_lane_fires_for_two_lanes_at_once_and_for_a_slower_mismatch() {
    let runs = BTreeMap::new();
    let leases = BTreeMap::new();
    let mut records = BTreeMap::new();
    records.insert("mismatched".to_string(), checking_record(5));
    let running = Running::default();
    let mut pool = pool(4);
    take_for_item(&mut pool, 1, "doubled");
    take_for_item(&mut pool, 2, "doubled");
    let actions = Vec::new();
    let recent = Vec::new();
    let input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);

    let found = check(&input);

    let doubled = found.iter().find(|f| f.id == "double_lane:doubled").expect("doubled");
    assert_eq!(doubled.threshold_minutes, 0);
    let mismatched = found
        .iter()
        .find(|f| f.id == "double_lane:mismatched")
        .expect("mismatched");
    assert_eq!(mismatched.threshold_minutes, LANE_MISMATCH_MINUTES);
}

#[test]
fn double_lane_says_nothing_about_an_item_whose_lane_agrees() {
    let runs = BTreeMap::new();
    let leases = BTreeMap::new();
    let mut records = BTreeMap::new();
    records.insert("fine".to_string(), checking_record(1));
    let running = Running::default();
    let mut pool = pool(4);
    take_for_item(&mut pool, 1, "fine");
    let actions = Vec::new();
    let recent = Vec::new();
    let input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);

    assert!(check(&input).is_empty());
}

#[test]
fn idle_lanes_fires_only_while_unpaused_with_a_free_lane_and_ready_work() {
    let runs = BTreeMap::new();
    let leases = BTreeMap::new();
    let records = BTreeMap::new();
    let running = Running::default();
    let pool = pool(2);
    let recent = Vec::new();

    let ready = vec![Action::AskForWork];
    let mut input = make_input(&runs, &leases, &records, &running, &pool, &ready, &recent);
    let found = check(&input);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "idle_lanes");

    input.paused = true;
    assert!(check(&input).is_empty());

    let nothing_ready: Vec<Action> = Vec::new();
    let input = make_input(&runs, &leases, &records, &running, &pool, &nothing_ready, &recent);
    assert!(check(&input).is_empty());
}

#[test]
fn repeat_error_fires_at_three_in_the_window_grouped_by_stripped_numbers() {
    let runs = BTreeMap::new();
    let leases = BTreeMap::new();
    let records = BTreeMap::new();
    let running = Running::default();
    let pool = pool(2);
    let actions = Vec::new();
    let errors = [
        event::Error {
            item: Some("R-12".into()),
            r#where: Some("gh".into()),
            error: Some("PR 101 not found".into()),
            ..Default::default()
        },
        event::Error {
            item: Some("R-12".into()),
            r#where: Some("gh".into()),
            error: Some("PR 202 not found".into()),
            ..Default::default()
        },
        event::Error {
            item: Some("R-12".into()),
            r#where: Some("gh".into()),
            error: Some("PR 303 not found".into()),
            ..Default::default()
        },
    ]
    .map(Event::Error);
    let recent: Vec<RecentEvent> = errors.iter().map(|e| RecentEvent { at_ms: 0, event: e }).collect();
    let input = Input {
        now_ms: 10_000,
        ..make_input(&runs, &leases, &records, &running, &pool, &actions, &recent)
    };

    let found = check(&input);

    let repeat = found
        .iter()
        .find(|f| f.id == "repeat_error:R-12")
        .expect("repeat_error");
    assert_eq!(repeat.threshold_minutes, 0);
}

#[test]
fn repeat_error_fires_at_once_for_host_call_undefined() {
    let runs = BTreeMap::new();
    let leases = BTreeMap::new();
    let records = BTreeMap::new();
    let running = Running::default();
    let pool = pool(2);
    let actions = Vec::new();
    let one = Event::HostCallUndefined(event::HostCallUndefined {
        cmd: Some("gh".into()),
        ..Default::default()
    });
    let recent = [RecentEvent { at_ms: 0, event: &one }];
    let input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);

    let found = check(&input);

    assert!(found.iter().any(|f| f.id == "repeat_error:host_call_undefined"));
}

#[test]
fn repeat_error_ignores_errors_outside_the_window() {
    let runs = BTreeMap::new();
    let leases = BTreeMap::new();
    let records = BTreeMap::new();
    let running = Running::default();
    let pool = pool(2);
    let actions = Vec::new();
    let old = Event::Error(event::Error {
        item: Some("R-12".into()),
        error: Some("boom".into()),
        ..Default::default()
    });
    let recent = [RecentEvent { at_ms: 0, event: &old }];
    let input = Input {
        now_ms: (REPEAT_WINDOW_MINUTES + 1) * 60_000,
        ..make_input(&runs, &leases, &records, &running, &pool, &actions, &recent)
    };

    assert!(check(&input).is_empty());
}

#[test]
fn no_progress_fires_for_checking_with_a_lane_and_nothing_running() {
    let mut runs = BTreeMap::new();
    runs.insert("R-1".to_string(), leased_run(ItemState::Checking));
    let leases = BTreeMap::new();
    let records = BTreeMap::new();
    let running = Running::default();
    let mut pool = pool(2);
    take_for_item(&mut pool, 1, "R-1");
    let actions = Vec::new();
    let recent = Vec::new();
    let input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);

    let found = check(&input);

    assert!(found.iter().any(|f| f.id == "no_progress:R-1"));
}

#[test]
fn no_progress_waits_quietly_for_a_free_lane() {
    let mut runs = BTreeMap::new();
    runs.insert("R-1".to_string(), leased_run(ItemState::Checking));
    let leases = BTreeMap::new();
    let records = BTreeMap::new();
    let running = Running::default();
    let pool = pool(2);
    let actions = Vec::new();
    let recent = Vec::new();
    let input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);

    assert!(check(&input).is_empty());
}

#[test]
fn normalize_strips_runs_of_digits() {
    assert_eq!(normalize("PR 101 not found"), "PR # not found");
    assert_eq!(normalize("lease on R-12 moved on"), "lease on R-# moved on");
}

fn restart_view(at_ms: i64, waiting_on: &[&str]) -> RestartView {
    RestartView {
        at_ms,
        waiting_on: waiting_on.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn restart_wait_is_quiet_without_a_restart_or_under_15_minutes_then_fires_and_ends() {
    let (runs, leases, records, running, pool) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(0),
    );
    let (actions, recent) = (Vec::new(), Vec::new());
    let mut input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);
    input.now_ms = 100 * 60_000;
    assert!(check(&input).is_empty());

    input.restart = Some(restart_view(input.now_ms - RESTART_WAIT_MS + 1, &["a merge"]));
    assert!(check(&input).is_empty());

    input.restart = Some(restart_view(input.now_ms - RESTART_WAIT_MS, &["a merge", "a promote"]));
    let found = check(&input);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "restart_wait");
    assert_eq!(found[0].item, None);
    assert_eq!(found[0].threshold_minutes, 0);
    assert!(found[0].text.contains("a merge, a promote"), "{}", found[0].text);

    input.restart = None;
    assert!(check(&input).is_empty());
}

#[test]
fn idle_lanes_fires_while_a_restart_is_pending_even_with_no_ready_work() {
    let (runs, leases, records, running, pool) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(2),
    );
    let (actions, recent) = (Vec::new(), Vec::new());
    let mut input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);
    input.now_ms = 60_000;
    input.restart = Some(restart_view(0, &["a merge"]));
    let found = check(&input);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "idle_lanes");
    assert!(found[0].text.contains("restart") && found[0].text.contains("a merge"));

    input.paused = true;
    assert!(check(&input).is_empty());
}

#[test]
fn a_restart_wait_finding_writes_one_anomaly_start_event() {
    use crate::telemetry::debounce::Telemetry;
    use crate::telemetry::events::EventLog;
    let (runs, leases, records, running, pool) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(0),
    );
    let (actions, recent) = (Vec::new(), Vec::new());
    let mut input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);
    input.now_ms = RESTART_WAIT_MS;
    input.restart = Some(restart_view(0, &["a merge"]));
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();
    telemetry.minute(PROJECT, check(&input), input.now_ms, &mut log);
    let starts = log
        .recent()
        .iter()
        .filter_map(|e| match e.event() {
            Some(Event::Anomaly(a)) if a.phase.as_deref() == Some("start") => Some(a),
            _ => None,
        })
        .count();
    assert_eq!(starts, 1);
}
