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
    records.insert(
        "mismatched".to_string(),
        RunRecord {
            state: "checking".into(),
            lane: Some(5),
            impl_session_id: None,
            branch: None,
            slice: 1,
        },
    );
    let running = Running::default();
    let mut pool = pool(4);
    pool.take(
        1,
        Holder::Item {
            project: PROJECT.into(),
            item: "doubled".into(),
        },
    );
    pool.take(
        2,
        Holder::Item {
            project: PROJECT.into(),
            item: "doubled".into(),
        },
    );
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
    records.insert(
        "fine".to_string(),
        RunRecord {
            state: "checking".into(),
            lane: Some(1),
            impl_session_id: None,
            branch: None,
            slice: 1,
        },
    );
    let running = Running::default();
    let mut pool = pool(4);
    pool.take(
        1,
        Holder::Item {
            project: PROJECT.into(),
            item: "fine".into(),
        },
    );
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
    pool.take(
        1,
        Holder::Item {
            project: PROJECT.into(),
            item: "R-1".into(),
        },
    );
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
