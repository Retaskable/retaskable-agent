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
        cant_start: None,
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

fn store_error(op: &str) -> Event {
    Event::StoreError(event::StoreError {
        op: Some(op.into()),
        ..Default::default()
    })
}

/// Runs `check` over store errors `(at_ms, op)` at `now_ms`, returning the sync_failing finding if any.
fn sync_finding(errors: &[(i64, &str)], now_ms: i64) -> Option<Found> {
    let events: Vec<Event> = errors.iter().map(|(_, op)| store_error(op)).collect();
    let recent: Vec<RecentEvent> = errors
        .iter()
        .zip(&events)
        .map(|((at_ms, _), event)| RecentEvent { at_ms: *at_ms, event })
        .collect();
    let (runs, leases, records) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (running, pool, actions) = (Running::default(), pool(2), Vec::new());
    let input = Input {
        now_ms,
        ..make_input(&runs, &leases, &records, &running, &pool, &actions, &recent)
    };
    check(&input).into_iter().find(|f| f.rule == "sync_failing")
}

#[test]
fn sync_failing_needs_three_matching_failures() {
    let two = [(0, "details_put"), (0, "draft_save")];
    assert!(sync_finding(&two, 0).is_none());

    let three = [(0, "details_put"), (0, "draft_save"), (0, "ask_post")];
    let f = sync_finding(&three, 0).expect("fires");
    assert_eq!(f.item, None);
    assert_eq!(f.threshold_minutes, 0);
    assert_eq!(f.details["count"], 3);
    assert_eq!(f.details["ops"], json!(["ask_post", "details_put", "draft_save"]));
}

#[test]
fn sync_failing_ignores_other_ops() {
    let errors = [(0, "list"), (0, "report"), (0, "claim_next"), (0, "settings_put")];
    assert!(sync_finding(&errors, 0).is_none());
}

#[test]
fn sync_failing_counts_only_the_window_and_ends_when_it_empties() {
    let min = 60_000;
    let spread = [(0, "details_a"), (6 * min, "draft_b"), (11 * min, "settings_c")];
    assert!(sync_finding(&spread, 11 * min).is_none());

    let close = [(2 * min, "details_a"), (6 * min, "draft_b"), (11 * min, "settings_c")];
    assert!(sync_finding(&close, 11 * min).is_some());
    // Later, the first has aged out of the window: the anomaly ends.
    assert!(sync_finding(&close, 13 * min).is_none());
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

#[test]
fn normalize_strips_git_shas() {
    assert_eq!(normalize("merge a1b2c3d failed"), normalize("merge 9f8e7d6c5b failed"));
    assert_eq!(normalize("deadbeef is not a hash"), "deadbeef is not a hash");
}

#[test]
fn a_cant_start_reason_keeps_idle_lanes_quiet_until_it_clears() {
    let (runs, leases, records, running, pool, ready, recent) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(2),
        vec![Action::AskForWork],
        Vec::new(),
    );
    let mut input = make_input(&runs, &leases, &records, &running, &pool, &ready, &recent);
    assert!(check(&input).iter().any(|f| f.id == "idle_lanes"));
    input.cant_start = Some("usage limit");
    assert!(check(&input).is_empty());
    input.cant_start = None;
    assert!(check(&input).iter().any(|f| f.id == "idle_lanes"));
}

#[test]
fn a_cant_start_reason_keeps_no_progress_quiet() {
    let mut runs = BTreeMap::new();
    runs.insert("R-1".to_string(), leased_run(ItemState::Checking));
    let (leases, records, running, mut pool, actions, recent) = (
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(2),
        Vec::new(),
        Vec::new(),
    );
    take_for_item(&mut pool, 1, "R-1");
    let mut input = make_input(&runs, &leases, &records, &running, &pool, &actions, &recent);
    assert!(check(&input).iter().any(|f| f.id == "no_progress:R-1"));
    input.cant_start = Some("usage limit");
    assert!(check(&input).is_empty());
    input.cant_start = None;
    assert!(check(&input).iter().any(|f| f.id == "no_progress:R-1"));
}

/// Runs `body` against an input with no runs, leases, records, actions or events and a pool of `lanes`.
fn with_empty_input(lanes: u32, body: impl FnOnce(Input)) {
    let (runs, leases, records, running, pool) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(lanes),
    );
    let (actions, recent) = (Vec::new(), Vec::new());
    body(make_input(&runs, &leases, &records, &running, &pool, &actions, &recent));
}

fn restart_view(at_ms: i64, waiting_on: &[&str]) -> RestartView {
    RestartView {
        at_ms,
        waiting_on: waiting_on.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn restart_wait_is_quiet_without_a_restart_or_under_15_minutes_then_fires_and_ends() {
    with_empty_input(0, |mut input| {
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
    });
}

#[test]
fn idle_lanes_fires_while_a_restart_is_pending_even_with_no_ready_work() {
    with_empty_input(2, |mut input| {
        input.now_ms = 60_000;
        input.restart = Some(restart_view(0, &["a merge"]));
        let found = check(&input);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "idle_lanes");
        assert!(found[0].text.contains("restart") && found[0].text.contains("a merge"));

        input.paused = true;
        assert!(check(&input).is_empty());
    });
}

#[test]
fn a_restart_wait_finding_writes_one_anomaly_start_event() {
    use crate::telemetry::debounce::Telemetry;
    use crate::telemetry::events::EventLog;
    with_empty_input(0, |mut input| {
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
    });
}

fn step(start: bool, item: &str, key: Option<&str>) -> Event {
    let (item, step, key) = (
        Some(item.to_string()),
        Some("review".to_string()),
        key.map(String::from),
    );
    if start {
        Event::StepStart(event::StepStart {
            item,
            step,
            key,
            ..Default::default()
        })
    } else {
        Event::StepEnd(event::StepEnd {
            item,
            step,
            key,
            ..Default::default()
        })
    }
}

/// The `duplicate_step` findings for events `(at_ms, event)` seen at `now_ms`.
fn duplicates(events: &[(i64, Event)], now_ms: i64) -> Vec<Found> {
    let recent: Vec<RecentEvent> = events
        .iter()
        .map(|(at_ms, event)| RecentEvent { at_ms: *at_ms, event })
        .collect();
    with_recent(&recent, now_ms)
}

fn with_recent(recent: &[RecentEvent], now_ms: i64) -> Vec<Found> {
    let (runs, leases, records, running, pool) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        Running::default(),
        pool(0),
    );
    let actions = Vec::new();
    let input = Input {
        now_ms,
        ..make_input(&runs, &leases, &records, &running, &pool, &actions, recent)
    };
    check(&input)
}

#[test]
fn duplicate_step_fires_for_two_starts_with_no_end_and_is_gone_after_30_minutes() {
    let events = [
        (0, step(true, "R-12", Some("slice 1 round 1"))),
        (60_000, step(true, "R-12", Some("slice 1 round 1"))),
    ];
    let found = duplicates(&events, 120_000);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "duplicate_step:R-12:review:slice 1 round 1");
    assert_eq!(found[0].item.as_deref(), Some("R-12"));
    assert_eq!(found[0].threshold_minutes, 0);
    assert_eq!(
        duplicates(&events, 60_000 + DUPLICATE_SHOWN_MINUTES * 60_000 - 1).len(),
        1
    );
    assert!(duplicates(&events, 60_000 + DUPLICATE_SHOWN_MINUTES * 60_000).is_empty());
}

#[test]
fn duplicate_step_ignores_start_end_start_different_keys_and_keyless_starts() {
    let k = Some("round 1");
    assert!(
        duplicates(
            &[
                (0, step(true, "R-1", k)),
                (1, step(false, "R-1", k)),
                (2, step(true, "R-1", k))
            ],
            3
        )
        .is_empty()
    );
    assert!(duplicates(&[(0, step(true, "R-1", k)), (1, step(true, "R-1", Some("round 2")))], 3).is_empty());
    assert!(duplicates(&[(0, step(true, "R-1", None)), (1, step(true, "R-1", None))], 3).is_empty());
}
