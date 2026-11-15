use serde_json::json;

use super::*;

const NOW: i64 = 1_700_000_000;

#[test]
fn the_week_is_the_first_reading_with_a_figure_and_a_reset_ahead() {
    let status_only = json!({ "rateLimitType": "seven_day", "status": "allowed" });
    let week = json!({ "rateLimitType": "seven_day", "utilization": 0.4567, "resetsAt": NOW + 3600 });
    let w = WeekUse::of([&status_only, &week], NOW).unwrap();
    assert_eq!((w.used, w.resets_at), (0.46, (NOW + 3600) * 1000));
}

#[test]
fn no_week_when_none_is_read_the_week_has_reset_or_the_figure_is_negative() {
    assert_eq!(WeekUse::of([], NOW), None);
    let reset = json!({ "rateLimitType": "seven_day", "utilization": 0.4, "resetsAt": NOW - 1 });
    assert_eq!(WeekUse::of([&reset], NOW), None);
    let over = json!({ "rateLimitType": "seven_day", "utilization": -0.5, "resetsAt": NOW + 1 });
    assert_eq!(WeekUse::of([&over], NOW), None);
}

#[test]
fn week_serializes_as_used_and_resets_at() {
    let w = WeekUse {
        used: 0.5,
        resets_at: 9,
    };
    assert_eq!(serde_json::to_value(w).unwrap(), json!({ "used": 0.5, "resetsAt": 9 }));
}

#[test]
fn a_passed_answer_names_the_computer_and_the_retry() {
    let a = json!({ "passed": { "to": "box-2", "retryMs": 60000 } });
    let p = Passed::of(a.as_object().unwrap()).unwrap();
    assert_eq!((p.to.as_str(), p.retry_ms), ("box-2", 60_000));
    assert_eq!(Passed::of(json!({}).as_object().unwrap()), None);
}
