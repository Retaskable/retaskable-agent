//! One event as a line of the `why` and `agent-report` output.
use serde_json::Value;

use super::{ms_of, span, str_of, truthy, usd_of};

/// One event as a line's text, after its time.
pub(super) fn describe(e: &Value) -> String {
    let get = |k: &str| e.get(k);
    let lane = match get("lane").and_then(Value::as_f64) {
        Some(n) => format!(" [lane {}]", n as i64),
        None => String::new(),
    };
    let kind = get("kind").and_then(Value::as_str).unwrap_or("");
    match kind {
        "item_state" => {
            let note = if truthy(get("note")) {
                format!(": {}", str_of(get("note")))
            } else {
                String::new()
            };
            format!("{} → {}{note}{lane}", str_of(get("from")), str_of(get("to")))
        }
        "lane_taken" | "lane_freed" => format!(
            "{} lane {} ({})",
            if kind == "lane_taken" { "took" } else { "freed" },
            str_of(get("lane")),
            str_of(get("why"))
        ),
        "lease" => format!("lease {} ({})", str_of(get("change")), str_of(get("state"))),
        "session_start" => {
            let scout = if truthy(get("scout")) {
                format!(" {}", str_of(get("scout")))
            } else {
                String::new()
            };
            let suffix = if truthy(get("attached")) {
                ", picked up after a restart"
            } else if truthy(get("detached")) {
                ", detached"
            } else {
                ""
            };
            format!(
                "{}{scout} started on {} ({}){lane}{suffix}",
                str_of(get("role")),
                str_of(get("model")),
                str_of(get("engine"))
            )
        }
        "session_end" => {
            let scout = if truthy(get("scout")) {
                format!(" {}", str_of(get("scout")))
            } else {
                String::new()
            };
            let cost = if truthy(get("cost")) {
                format!(" {}", usd_of(get("cost")))
            } else {
                String::new()
            };
            let error = if truthy(get("error")) {
                format!(": {}", str_of(get("error")))
            } else {
                String::new()
            };
            let outcome = if truthy(get("ok")) {
                "ok".to_string()
            } else {
                str_of(get("subtype"))
            };
            format!(
                "{}{scout} ended {outcome} after {}{cost}{error}",
                str_of(get("role")),
                span(ms_of(get("ms")))
            )
        }
        "step_start" => format!(
            "{} {} started{}",
            str_of(get("step")),
            str_of(get("key")),
            if truthy(get("again")) {
                " AGAIN without finishing"
            } else {
                ""
            }
        ),
        "step_end" => {
            let checks = match get("checks").and_then(Value::as_array) {
                Some(arr) if !arr.is_empty() => format!(
                    " ({})",
                    arr.iter()
                        .map(|c| format!(
                            "{} {}",
                            str_of(c.get("name")),
                            if truthy(c.get("ok")) { "✓" } else { "✗" }
                        ))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                _ => String::new(),
            };
            let outcome = if truthy(get("ok")) {
                "ok".to_string()
            } else {
                let o = str_of(get("outcome"));
                if o.is_empty() { "not ok".to_string() } else { o }
            };
            format!(
                "{} {} ended {outcome} after {}{checks}",
                str_of(get("step")),
                str_of(get("key")),
                span(ms_of(get("ms")))
            )
        }
        "ci" => {
            let ms_part = match get("ms").and_then(Value::as_f64) {
                Some(n) => format!(" after {}", span(n)),
                None => String::new(),
            };
            format!("CI {}{ms_part}", str_of(get("state")))
        }
        "merged" => format!("merged PR #{}", str_of(get("pr"))),
        "error" => format!("error in {}: {}", str_of(get("where")), str_of(get("error"))),
        "store_error" => {
            let code = if truthy(get("code")) {
                format!(" ({})", str_of(get("code")))
            } else {
                String::new()
            };
            format!("store {} failed{code}: {}", str_of(get("op")), str_of(get("error")))
        }
        "claim" => format!("claim answered: {}", str_of(get("answer"))),
        "anomaly" => format!(
            "{}{}",
            if get("phase").and_then(Value::as_str) == Some("end") {
                "no longer: "
            } else {
                "LOOKS WRONG: "
            },
            str_of(get("text"))
        ),
        _ => e
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter(|(k, _)| !matches!(k.as_str(), "at" | "kind" | "project" | "item"))
                    .map(|(k, v)| {
                        let vs = if v.is_object() || v.is_array() {
                            v.to_string()
                        } else {
                            str_of(Some(v))
                        };
                        format!("{k}={vs}")
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::describe;

    #[test]
    fn item_state_adds_a_note_and_a_lane_only_when_present() {
        assert_eq!(
            describe(&json!({ "kind": "item_state", "from": "queued", "to": "implementing" })),
            "queued → implementing"
        );
        assert_eq!(
            describe(&json!({
                "kind": "item_state", "from": "checking", "to": "implementing",
                "note": "Blocking checks failed", "lane": 1,
            })),
            "checking → implementing: Blocking checks failed [lane 1]"
        );
    }

    #[test]
    fn lane_taken_and_lane_freed_name_the_lane_and_the_reason() {
        assert_eq!(
            describe(&json!({ "kind": "lane_taken", "lane": 1, "why": "implementing" })),
            "took lane 1 (implementing)"
        );
        assert_eq!(
            describe(&json!({ "kind": "lane_freed", "lane": 1, "why": "merged" })),
            "freed lane 1 (merged)"
        );
    }

    #[test]
    fn lease_names_the_change_and_the_state() {
        assert_eq!(
            describe(&json!({ "kind": "lease", "change": "renewed", "state": "held" })),
            "lease renewed (held)"
        );
    }

    #[test]
    fn session_start_adds_the_scout_and_a_restart_or_detached_suffix() {
        assert_eq!(
            describe(&json!({ "kind": "session_start", "role": "implementer", "model": "opus", "engine": "claude" })),
            "implementer started on opus (claude)"
        );
        assert_eq!(
            describe(&json!({
                "kind": "session_start", "role": "scout", "scout": "tests", "model": "opus",
                "engine": "claude", "lane": 2, "attached": true,
            })),
            "scout tests started on opus (claude) [lane 2], picked up after a restart"
        );
        assert_eq!(
            describe(&json!({
                "kind": "session_start", "role": "implementer", "model": "opus", "engine": "claude",
                "detached": true,
            })),
            "implementer started on opus (claude), detached"
        );
    }

    #[test]
    fn session_end_adds_the_scout_cost_and_error_only_when_present() {
        assert_eq!(
            describe(&json!({ "kind": "session_end", "role": "implementer", "ms": 60_000, "ok": true })),
            "implementer ended ok after 1m"
        );
        assert_eq!(
            describe(&json!({
                "kind": "session_end", "role": "scout", "scout": "spec", "ms": 60_000,
                "ok": false, "subtype": "error", "cost": 0.5, "error": "timed out",
            })),
            "scout spec ended error after 1m $0.50: timed out"
        );
    }

    #[test]
    fn step_start_marks_a_repeat_without_finishing() {
        assert_eq!(
            describe(&json!({ "kind": "step_start", "step": "checks", "key": "round 1" })),
            "checks round 1 started"
        );
        assert_eq!(
            describe(&json!({ "kind": "step_start", "step": "checks", "key": "round 1", "again": true })),
            "checks round 1 started AGAIN without finishing"
        );
    }

    #[test]
    fn step_end_without_checks_or_ok_falls_back_to_the_outcome_or_not_ok() {
        assert_eq!(
            describe(&json!({ "kind": "step_end", "step": "review", "key": "round 1", "ms": 60_000, "ok": true })),
            "review round 1 ended ok after 1m"
        );
        assert_eq!(
            describe(&json!({
                "kind": "step_end", "step": "review", "key": "round 1", "ms": 60_000,
                "ok": false, "outcome": "changes_requested",
            })),
            "review round 1 ended changes_requested after 1m"
        );
        assert_eq!(
            describe(&json!({ "kind": "step_end", "step": "review", "key": "round 1", "ms": 60_000, "ok": false })),
            "review round 1 ended not ok after 1m"
        );
    }

    #[test]
    fn ci_adds_how_long_it_took_only_when_given() {
        assert_eq!(describe(&json!({ "kind": "ci", "state": "passed" })), "CI passed");
        assert_eq!(
            describe(&json!({ "kind": "ci", "state": "passed", "ms": 60_000 })),
            "CI passed after 1m"
        );
    }

    #[test]
    fn merged_names_the_pr() {
        assert_eq!(describe(&json!({ "kind": "merged", "pr": 41 })), "merged PR #41");
    }

    #[test]
    fn error_names_where_and_what() {
        assert_eq!(
            describe(&json!({ "kind": "error", "where": "checks", "error": "timeout" })),
            "error in checks: timeout"
        );
    }

    #[test]
    fn store_error_adds_the_code_only_when_present() {
        assert_eq!(
            describe(&json!({ "kind": "store_error", "op": "claim", "error": "locked" })),
            "store claim failed: locked"
        );
        assert_eq!(
            describe(&json!({ "kind": "store_error", "op": "claim", "error": "locked", "code": "E_LOCK" })),
            "store claim failed (E_LOCK): locked"
        );
    }

    #[test]
    fn claim_names_the_answer() {
        assert_eq!(
            describe(&json!({ "kind": "claim", "answer": "yes" })),
            "claim answered: yes"
        );
    }

    #[test]
    fn anomaly_marks_the_end_phase_differently_from_the_start() {
        assert_eq!(
            describe(&json!({ "kind": "anomaly", "phase": "start", "text": "stuck" })),
            "LOOKS WRONG: stuck"
        );
        assert_eq!(
            describe(&json!({ "kind": "anomaly", "phase": "end", "text": "stuck" })),
            "no longer: stuck"
        );
    }

    #[test]
    fn an_unknown_kind_falls_back_to_its_other_fields_sorted_by_key() {
        assert_eq!(
            describe(
                &json!({ "kind": "tick", "at": "t", "project": "p", "item": "i", "status": "running", "lanes": 2 })
            ),
            "lanes=2 status=running"
        );
    }
}
