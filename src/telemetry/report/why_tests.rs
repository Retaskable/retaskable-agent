use serde_json::json;

use super::*;

fn start(id: &str, at: &str) -> Value {
    json!({ "kind": "anomaly", "phase": "start", "id": id, "rule": "no_progress", "text": "stuck", "at": at })
}

fn end(id: &str, at: &str) -> Value {
    json!({ "kind": "anomaly", "phase": "end", "id": id, "at": at })
}

#[test]
fn anomaly_spans_pairs_start_and_end_by_id_skips_other_kinds_and_leaves_unmatched_starts_open() {
    let events = vec![
        json!({ "kind": "tick", "at": "2026-10-15T07:59:00.000Z" }),
        start("a", "2026-10-15T08:00:00.000Z"),
        start("b", "2026-10-15T08:01:00.000Z"),
        end("a", "2026-10-15T08:05:00.000Z"),
        end("unknown", "2026-10-15T08:06:00.000Z"),
    ];
    let spans = anomaly_spans(&events);
    assert_eq!(spans.len(), 2, "the tick and the unmatched end id are not spans");
    assert_eq!(spans[0].start, "2026-10-15T08:00:00.000Z");
    assert_eq!(spans[0].end, Some("2026-10-15T08:05:00.000Z".to_string()));
    assert_eq!(spans[1].start, "2026-10-15T08:01:00.000Z");
    assert_eq!(spans[1].end, None, "b's end never came");
}

#[test]
fn anomaly_spans_keeps_the_earlier_start_open_when_the_same_id_starts_again_before_ending() {
    let events = vec![
        start("a", "2026-10-15T08:00:00.000Z"),
        start("a", "2026-10-15T08:10:00.000Z"),
        end("a", "2026-10-15T08:15:00.000Z"),
    ];
    let spans = anomaly_spans(&events);
    assert_eq!(spans.len(), 2);
    assert_eq!(
        spans[0].end, None,
        "the first span is orphaned once its id is re-opened"
    );
    assert_eq!(
        spans[1].end,
        Some("2026-10-15T08:15:00.000Z".to_string()),
        "the end pairs with the most recently opened span for that id"
    );
}

#[test]
fn anomaly_line_formats_a_closed_span_with_or_without_the_date() {
    let events = vec![
        start("a", "2026-10-15T08:00:00.000Z"),
        end("a", "2026-10-15T08:05:00.000Z"),
    ];
    let span = &anomaly_spans(&events)[0];
    assert_eq!(
        anomaly_line(span, true),
        "  2026-10-15 08:00 → 08:05 (5m)  no_progress: stuck"
    );
    assert_eq!(anomaly_line(span, false), "  08:00 → 08:05 (5m)  no_progress: stuck");
}

#[test]
fn anomaly_line_reports_a_still_open_span_as_such() {
    let events = vec![start("a", "2026-10-15T08:00:00.000Z")];
    let span = &anomaly_spans(&events)[0];
    assert_eq!(
        anomaly_line(span, true),
        "  2026-10-15 08:00 → still open  no_progress: stuck"
    );
    assert_eq!(anomaly_line(span, false), "  08:00 → still open  no_progress: stuck");
}
