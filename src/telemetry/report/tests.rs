use std::collections::HashMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::telemetry::events::{iso_utc_millis, parse_iso_ms};

use super::*;
use crate::telemetry::event::Entry;

const T0_TEXT: &str = "2026-10-15T08:00:00.000Z";

fn t0() -> i64 {
    parse_iso_ms(T0_TEXT).unwrap()
}

fn at(min: i64) -> String {
    iso_utc_millis(t0() + min * 60_000)
}

fn write_jsonl(dir: &Path, events: &[Value]) {
    let mut by_day: HashMap<String, Vec<Value>> = HashMap::new();
    for e in events {
        let at = str_of(e.get("at"));
        by_day.entry(at[0..10].to_string()).or_default().push(e.clone());
    }
    for (day, mut es) in by_day {
        es.sort_by_key(|a| str_of(a.get("at")));
        std::fs::create_dir_all(dir).unwrap();
        let mut text = String::new();
        for e in es {
            text.push_str(&serde_json::to_string(&e).unwrap());
            text.push('\n');
        }
        std::fs::write(dir.join(format!("{day}.jsonl")), text).unwrap();
    }
}

/// A day of one item's life, a second project's few events, and the minute summaries around them.
fn fixture() -> tempfile::TempDir {
    let data = tempfile::tempdir().unwrap();
    let tick = |m: i64| {
        json!({
            "at": at(m), "kind": "tick", "status": "running", "lanes": 2,
            "busy": if m < 20 { json!([1]) } else if m < 40 { json!([]) } else { json!([1, 2]) },
            "sessions": 1,
        })
    };
    let ticks: Vec<Value> = (0..60).map(tick).collect();
    let mut alpha = vec![
        ticks[0].clone(),
        json!({ "at": at(0), "kind": "item_state", "item": "R-12", "from": "queued", "to": "implementing", "note": "Slice 1 started in lane 1 on retaskable/r-12", "lane": 1 }),
        json!({ "at": at(0), "kind": "lane_taken", "item": "R-12", "lane": 1, "why": "implementing: Slice 1 started in lane 1" }),
        json!({ "at": at(1), "kind": "session_start", "item": "R-12", "lane": 1, "role": "implementer", "engine": "claude", "model": "opus", "session": "s-1" }),
    ];
    alpha.extend(ticks[1..20].iter().cloned());
    alpha.extend([
        json!({ "at": at(20), "kind": "session_end", "item": "R-12", "lane": 1, "role": "implementer", "engine": "claude", "model": "opus", "session": "s-1", "ms": 19 * 60_000, "cost": 1.25, "ok": true, "subtype": "success" }),
        json!({ "at": at(20), "kind": "item_state", "item": "R-12", "from": "implementing", "to": "checking", "lane": 1 }),
        json!({ "at": at(21), "kind": "step_start", "item": "R-12", "step": "checks", "key": "slice 1 round 1" }),
        json!({ "at": at(25), "kind": "step_end", "item": "R-12", "step": "checks", "key": "slice 1 round 1", "ms": 4 * 60_000, "ok": false, "checks": [{ "name": "test", "ok": false, "sec": 200 }] }),
        json!({ "at": at(25), "kind": "item_state", "item": "R-12", "from": "checking", "to": "implementing", "note": "Blocking checks failed: test", "lane": 1 }),
        json!({ "at": at(26), "kind": "anomaly", "phase": "start", "id": "no_progress:R-12", "rule": "no_progress", "item": "R-12", "text": "R-12 is implementing with nothing running and nothing new for 20m" }),
    ]);
    alpha.extend(ticks[20..60].iter().cloned());
    alpha.extend([
        json!({ "at": at(60 + 481), "kind": "anomaly", "phase": "end", "id": "no_progress:R-12", "rule": "no_progress", "item": "R-12", "text": "R-12 is implementing with nothing running and nothing new for 8h" }),
        json!({ "at": at(60 + 481), "kind": "item_state", "item": "R-12", "from": "implementing", "to": "reviewing", "lane": 1 }),
        json!({ "at": at(60 + 490), "kind": "item_state", "item": "R-12", "from": "reviewing", "to": "merging", "note": "Opus review: approved" }),
        json!({ "at": at(60 + 495), "kind": "merged", "item": "R-12", "pr": 41 }),
        json!({ "at": at(60 + 495), "kind": "item_state", "item": "R-12", "from": "merging", "to": "merged", "note": "Merged PR #41" }),
        json!({ "at": at(30), "kind": "restart_asked", "reason": "asked", "by": "james", "waitingOn": ["1 session"] }),
        json!({ "at": at(45), "kind": "restart_exit", "reason": "asked", "by": "james", "drainMs": 15 * 60_000 }),
        json!({ "at": at(47), "kind": "restart_back", "reason": "asked", "by": "james", "downMs": 2 * 60_000 }),
    ]);
    alpha.sort_by_key(|a| str_of(a.get("at")));
    write_jsonl(&data.path().join("projects").join("alpha").join("events"), &alpha);
    let beta = vec![
        json!({ "at": at(30), "kind": "restart_asked", "reason": "asked", "by": "james" }),
        json!({ "at": at(45), "kind": "restart_exit", "reason": "asked", "by": "james", "drainMs": 15 * 60_000 }),
        json!({ "at": at(47), "kind": "restart_back", "reason": "asked", "by": "james", "downMs": 2 * 60_000 }),
        json!({ "at": at(50), "kind": "item_state", "item": "B-1", "from": "queued", "to": "implementing" }),
    ];
    write_jsonl(&data.path().join("projects").join("beta").join("events"), &beta);
    data
}

#[test]
fn since_takes_an_iso_time_or_a_span_back_from_now() {
    let t0 = t0();
    assert_eq!(parse_since("90m", t0), Some(t0 - 90 * 60_000));
    assert_eq!(parse_since("6h", t0), Some(t0 - 6 * 3_600_000));
    assert_eq!(parse_since("2d", t0), Some(t0 - 2 * 86_400_000));
    assert_eq!(parse_since("2026-10-15T08:00:00Z", 0), Some(t0));
    assert_eq!(parse_since("yesterday-ish", t0), None);
}

#[test]
fn find_item_skips_flags_and_their_values_and_a_bare_double_dash() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let r12 = Some("R-12".to_string());
    assert_eq!(find_item(&args(&["--", "R-12"])), r12);
    assert_eq!(find_item(&args(&["--since", "6h", "R-12"])), r12);
    assert_eq!(find_item(&args(&["--since=6h", "R-12"])), r12);
    assert_eq!(find_item(&args(&["--", "R-12", "--project", "alpha"])), r12);
    assert_eq!(find_item(&args(&["--", "--project", "alpha"])), None);
}

#[test]
fn read_all_rewrites_a_short_hand_edited_at_in_full_so_the_reports_can_slice_it() {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().join("projects").join("alpha").join("events");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("2026-10-15.jsonl"),
        "{\"at\":\"2026-10-15T8:0:0Z\",\"kind\":\"claim\",\"item\":\"R-1\"}
",
    )
    .unwrap();
    let events = read_all(data.path(), 0, i64::MAX, None);
    assert_eq!(events[0].at, "2026-10-15T08:00:00.000Z");
    assert!(why_text(&events, "R-1").contains("08:00:00"));
}

#[test]
fn read_all_reads_every_projects_days_in_order_a_project_or_a_window_when_asked() {
    let f = fixture();
    let all = read_all(f.path(), 0, i64::MAX, None);
    let mut projects: Vec<String> = all.iter().map(|e| e.project.clone().unwrap_or_default()).collect();
    projects.sort();
    projects.dedup();
    assert_eq!(projects, ["alpha", "beta"]);
    assert!(all.windows(2).all(|w| w[0].at <= w[1].at), "oldest first");
    assert!(
        read_all(f.path(), 0, i64::MAX, Some("beta"))
            .iter()
            .all(|e| e.project.as_deref() == Some("beta"))
    );
    let since60 = t0() + 60 * 60_000;
    assert!(
        read_all(f.path(), since60, i64::MAX, None)
            .iter()
            .all(|e| parse_iso_ms(&e.at).unwrap() >= since60)
    );
    assert_eq!(
        read_all(&f.path().join("nowhere"), 0, i64::MAX, None),
        Vec::<Entry>::new()
    );
}

#[test]
fn why_the_items_events_in_order_with_the_long_gap_marked_its_anomalies_and_its_sessions() {
    let f = fixture();
    let text = why_text(&read_all(f.path(), 0, i64::MAX, None), "R-12");
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(
        lines[0],
        "R-12 (alpha): 14 events, 2026-10-15 08:00:00 to 2026-10-15 17:15:00 UTC"
    );
    assert!(lines.contains(
        &"  08:00:00  item_state    queued → implementing: Slice 1 started in lane 1 on retaskable/r-12 [lane 1]"
    ));
    assert!(lines.contains(&"  08:25:00  step_end      checks slice 1 round 1 ended not ok after 4m (test ✗)"));
    assert!(lines.contains(&"            … nothing for 8h 35m"), "{text}");
    let gap = lines
        .iter()
        .position(|l| *l == "            … nothing for 8h 35m")
        .unwrap();
    assert!(lines[gap - 1].starts_with("  08:26:00  anomaly"));
    assert!(lines[gap + 1].starts_with("  17:01:00  anomaly       no longer: "));
    assert!(lines.contains(&"  2026-10-15 08:26 → 17:01 (8h 35m)  no_progress: R-12 is implementing with nothing running and nothing new for 20m"));
    assert!(
        lines.iter().any(|l| l.starts_with("  2026-10-15 08:20  implementer")
            && l.contains("opus")
            && l.contains("19m")
            && l.contains("$1.25")
            && l.trim_end().ends_with("ok")),
        "{text}"
    );
    assert!(!text.contains("B-1"), "another item's events aren't in it");
    assert_eq!(
        why_text(&read_all(f.path(), 0, i64::MAX, None), "R-404"),
        "Nothing in the event log for R-404."
    );
}

#[test]
fn agent_report_lane_use_idle_stretches_phase_medians_rework_by_why_restarts_once_and_anomalies() {
    let f = fixture();
    let from = t0();
    let to = t0() + 10 * 3_600_000;
    let text = report_text(&read_all(f.path(), from, to, None), from, to);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(
        lines[0],
        "Agent report, 2026-10-15 08:00 to 2026-10-15 18:00 UTC (10h 00m)"
    );
    assert!(
        lines.contains(&"  overall: 1.00 of 2.00 lanes busy on average (50%), over 60 minutes"),
        "{text}"
    );
    assert!(lines.contains(&"  lane 1: busy 67%"));
    assert!(lines.contains(&"  lane 2: busy 33%"));
    assert!(
        lines.contains(&"  idle 2026-10-15 08:20 → 08:40 (20m): every lane free"),
        "{text}"
    );
    assert!(
        lines.contains(&"  slices merged: 1, median 9h 15m from start to merge"),
        "{text}"
    );
    assert!(
        lines.contains(&"  implementing       median  4h 28m  (2 times)"),
        "{text}"
    );
    assert!(
        lines.contains(&"  checking           median      5m  (1 time)"),
        "{text}"
    );
    assert!(lines.contains(&"  rework rounds: 1"));
    assert!(lines.contains(&"      1  Blocking checks failed"));
    let restarts_at = lines.iter().position(|l| *l == "Restarts").unwrap();
    assert_eq!(
        &lines[restarts_at + 1..],
        ["  2026-10-15 08:30 asked by @james (asked): drained 15m, down 2m, back at 08:47"],
        "once, though both projects logged it"
    );
    assert!(lines.contains(&"== beta"));
    assert!(report_text(&[], from, to).ends_with("No events in this window."));
}
