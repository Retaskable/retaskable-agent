use super::*;
use crate::telemetry::watchdog::Found;

const PROJECT: &str = "proj";

fn found(id: &str, threshold_minutes: u32) -> Found {
    Found {
        id: id.to_string(),
        rule: "idle_lanes",
        item: None,
        text: format!("{id} is stuck"),
        details: serde_json::Value::Null,
        threshold_minutes,
    }
}

fn anomaly_entries(log: &EventLog) -> Vec<&Anomaly> {
    log.recent()
        .iter()
        .filter_map(|e| match e.event() {
            Some(Event::Anomaly(a)) => Some(a),
            _ => None,
        })
        .collect()
}

#[test]
fn a_finding_below_threshold_writes_no_event() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();

    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 0, &mut log);
    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 4 * 60_000, &mut log);

    assert!(anomaly_entries(&log).is_empty());
}

#[test]
fn crossing_the_threshold_writes_exactly_one_start_event() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();

    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 0, &mut log);
    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 5 * 60_000, &mut log);
    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 6 * 60_000, &mut log);

    let starts: Vec<_> = anomaly_entries(&log)
        .into_iter()
        .filter(|a| a.phase.as_deref() == Some("start"))
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].id.as_deref(), Some("idle_lanes"));
}

#[test]
fn a_finding_that_clears_writes_one_end_event_with_how_long_it_held() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();

    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 0, &mut log);
    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 5 * 60_000, &mut log);
    telemetry.minute(PROJECT, Vec::new(), 8 * 60_000, &mut log);

    let ends: Vec<_> = anomaly_entries(&log)
        .into_iter()
        .filter(|a| a.phase.as_deref() == Some("end"))
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].id.as_deref(), Some("idle_lanes"));
    assert_eq!(ends[0].ms, Some((8 * 60_000) as f64));
}

#[test]
fn a_finding_that_clears_before_it_started_writes_no_end_event() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();

    telemetry.minute(PROJECT, vec![found("idle_lanes", 5)], 0, &mut log);
    telemetry.minute(PROJECT, Vec::new(), 60_000, &mut log);

    assert!(anomaly_entries(&log).is_empty());
}

#[test]
fn a_zero_threshold_finding_starts_on_its_first_tick() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();

    telemetry.minute(PROJECT, vec![found("repeat_error:R-1", 0)], 0, &mut log);

    let starts: Vec<_> = anomaly_entries(&log)
        .into_iter()
        .filter(|a| a.phase.as_deref() == Some("start"))
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].id.as_deref(), Some("repeat_error:R-1"));
}

#[test]
fn open_anomalies_is_empty_below_the_threshold() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();
    telemetry.minute(PROJECT, vec![found("a", 5)], 0, &mut log);
    telemetry.minute(PROJECT, vec![found("a", 5)], 4 * 60_000, &mut log);
    assert!(telemetry.open_anomalies().is_empty());
}

#[test]
fn open_anomalies_lists_a_started_one_with_its_first_check_time_and_drops_it_when_it_ends() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();
    telemetry.minute(PROJECT, vec![found("a", 5)], 1_000, &mut log);
    telemetry.minute(PROJECT, vec![found("a", 5)], 1_000 + 5 * 60_000, &mut log);
    let open = telemetry.open_anomalies();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, "a");
    assert_eq!(open[0].rule, "idle_lanes");
    assert_eq!(open[0].text, "a is stuck");
    assert_eq!(open[0].since_ms, 1_000);
    telemetry.minute(PROJECT, vec![], 1_000 + 6 * 60_000, &mut log);
    assert!(telemetry.open_anomalies().is_empty());
}

#[test]
fn open_anomalies_are_ordered_by_since_then_id() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();
    telemetry.minute(PROJECT, vec![found("c", 0)], 0, &mut log);
    telemetry.minute(
        PROJECT,
        vec![found("c", 0), found("b", 0), found("a", 0)],
        60_000,
        &mut log,
    );
    let ids: Vec<_> = telemetry.open_anomalies().into_iter().map(|o| o.id).collect();
    assert_eq!(ids, ["c", "a", "b"]);
}

#[test]
fn a_zero_threshold_finding_is_listed_on_its_first_tick() {
    let mut telemetry = Telemetry::new();
    let mut log = EventLog::new();
    telemetry.minute(PROJECT, vec![found("a", 0)], 7, &mut log);
    assert_eq!(telemetry.open_anomalies()[0].since_ms, 7);
}
