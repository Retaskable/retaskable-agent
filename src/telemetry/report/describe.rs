//! One event as a line of the `why` and `agent-report` output.
use serde_json::{Map, Value};

use crate::telemetry::event::{Body, Entry, Event};

use super::{span, str_of};

fn text(o: &Option<String>) -> &str {
    o.as_deref().unwrap_or("")
}

fn flag(o: Option<bool>) -> bool {
    o == Some(true)
}

/// ` [lane N]` when the event names a lane.
fn lane_suffix(lane: Option<u32>) -> String {
    lane.map(|n| format!(" [lane {n}]")).unwrap_or_default()
}

/// `" "` and the scout's name, when there is one.
fn scout_part(scout: &Option<String>) -> String {
    match scout.as_deref() {
        Some(s) if !s.is_empty() => format!(" {s}"),
        _ => String::new(),
    }
}

/// `prefix` and the text, when the text is not empty.
fn with_prefix(prefix: &str, o: &Option<String>) -> String {
    match o.as_deref() {
        Some(s) if !s.is_empty() => format!("{prefix}{s}"),
        _ => String::new(),
    }
}

/// The outcome of a session or a step: `ok`, or the reason it was not.
fn outcome(ok: Option<bool>, otherwise: &str) -> String {
    if flag(ok) {
        "ok".to_string()
    } else {
        otherwise.to_string()
    }
}

/// One event as a line's text, after its time.
pub(super) fn describe(e: &Entry) -> String {
    match &e.body {
        Body::Known(event) => describe_event(event),
        Body::Other { fields, .. } => other_fields(fields),
    }
}

fn describe_event(event: &Event) -> String {
    match event {
        Event::ItemState(e) => format!(
            "{} → {}{}{}",
            text(&e.from),
            text(&e.to),
            with_prefix(": ", &e.note),
            lane_suffix(e.lane)
        ),
        Event::LaneTaken(e) => format!(
            "took lane {} ({})",
            e.lane.map(|n| n.to_string()).unwrap_or_default(),
            text(&e.why)
        ),
        Event::LaneFreed(e) => format!(
            "freed lane {} ({})",
            e.lane.map(|n| n.to_string()).unwrap_or_default(),
            text(&e.why)
        ),
        Event::Lease(e) => format!("lease {} ({})", text(&e.change), text(&e.state)),
        Event::SessionStart(e) => {
            let suffix = if flag(e.attached) {
                ", picked up after a restart"
            } else if flag(e.detached) {
                ", detached"
            } else {
                ""
            };
            format!(
                "{}{} started on {} ({}){}{suffix}",
                text(&e.role),
                scout_part(&e.scout),
                text(&e.model),
                text(&e.engine),
                lane_suffix(e.lane)
            )
        }
        Event::SessionEnd(e) => {
            let cost = match e.cost {
                Some(c) if c != 0.0 => format!(" ${c:.2}"),
                _ => String::new(),
            };
            format!(
                "{}{} ended {} after {}{cost}{}",
                text(&e.role),
                scout_part(&e.scout),
                outcome(e.ok, text(&e.subtype)),
                span(e.ms.unwrap_or(0.0)),
                with_prefix(": ", &e.error)
            )
        }
        Event::StepStart(e) => format!(
            "{} {} started{}",
            text(&e.step),
            text(&e.key),
            if flag(e.again) { " AGAIN without finishing" } else { "" }
        ),
        Event::StepEnd(e) => {
            let checks = match &e.checks {
                Some(checks) if !checks.is_empty() => format!(
                    " ({})",
                    checks
                        .iter()
                        .map(|c| format!("{} {}", text(&c.name), if flag(c.ok) { "✓" } else { "✗" }))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                _ => String::new(),
            };
            let not_ok = match text(&e.outcome) {
                "" => "not ok",
                o => o,
            };
            format!(
                "{} {} ended {} after {}{checks}",
                text(&e.step),
                text(&e.key),
                outcome(e.ok, not_ok),
                span(e.ms.unwrap_or(0.0))
            )
        }
        Event::Ci(e) => {
            let after = e.ms.map(|n| format!(" after {}", span(n))).unwrap_or_default();
            format!("CI {}{after}", text(&e.state))
        }
        Event::Merged(e) => format!("merged PR #{}", e.pr.map(|n| n.to_string()).unwrap_or_default()),
        Event::Error(e) => format!("error in {}: {}", text(&e.r#where), text(&e.error)),
        Event::StoreError(e) => {
            let code = match str_of(e.code.as_ref()) {
                c if c.is_empty() => String::new(),
                c => format!(" ({c})"),
            };
            format!("store {} failed{code}: {}", text(&e.op), text(&e.error))
        }
        Event::Claim(e) => format!("claim answered: {}", str_of(e.answer.as_ref())),
        Event::Anomaly(e) => format!(
            "{}{}",
            if e.phase.as_deref() == Some("end") {
                "no longer: "
            } else {
                "LOOKS WRONG: "
            },
            text(&e.text)
        ),
        // No line of their own: every field, sorted by name.
        Event::Tick(_)
        | Event::HostCallUndefined(_)
        | Event::RestartAsked(_)
        | Event::RestartWaiting(_)
        | Event::RestartExit(_)
        | Event::RestartBack(_) => other_fields(&event.fields()),
    }
}

/// `key=value` for every field but the ones the report line already shows, sorted by key.
fn other_fields(fields: &Map<String, Value>) -> String {
    fields
        .iter()
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
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::describe;
    use crate::telemetry::event::Entry;

    /// The line for an event given as JSON (a time is added when it has none).
    fn d(mut v: serde_json::Value) -> String {
        v["at"] = v
            .get("at")
            .cloned()
            .unwrap_or_else(|| json!("2026-10-15T08:00:00.000Z"));
        describe(&Entry::from_value(&v).expect("an event"))
    }

    #[test]
    fn item_state_adds_a_note_and_a_lane_only_when_present() {
        assert_eq!(
            d(json!({ "kind": "item_state", "from": "queued", "to": "implementing" })),
            "queued → implementing"
        );
        assert_eq!(
            d(json!({
                "kind": "item_state", "from": "checking", "to": "implementing",
                "note": "Blocking checks failed", "lane": 1,
            })),
            "checking → implementing: Blocking checks failed [lane 1]"
        );
    }

    #[test]
    fn lane_taken_and_lane_freed_name_the_lane_and_the_reason() {
        assert_eq!(
            d(json!({ "kind": "lane_taken", "lane": 1, "why": "implementing" })),
            "took lane 1 (implementing)"
        );
        assert_eq!(
            d(json!({ "kind": "lane_freed", "lane": 1, "why": "merged" })),
            "freed lane 1 (merged)"
        );
    }

    #[test]
    fn lease_names_the_change_and_the_state() {
        assert_eq!(
            d(json!({ "kind": "lease", "change": "renewed", "state": "held" })),
            "lease renewed (held)"
        );
    }

    #[test]
    fn session_start_adds_the_scout_and_a_restart_or_detached_suffix() {
        assert_eq!(
            d(json!({ "kind": "session_start", "role": "implementer", "model": "opus", "engine": "claude" })),
            "implementer started on opus (claude)"
        );
        assert_eq!(
            d(json!({
                "kind": "session_start", "role": "scout", "scout": "tests", "model": "opus",
                "engine": "claude", "lane": 2, "attached": true,
            })),
            "scout tests started on opus (claude) [lane 2], picked up after a restart"
        );
        assert_eq!(
            d(json!({
                "kind": "session_start", "role": "implementer", "model": "opus", "engine": "claude",
                "detached": true,
            })),
            "implementer started on opus (claude), detached"
        );
    }

    #[test]
    fn session_end_adds_the_scout_cost_and_error_only_when_present() {
        assert_eq!(
            d(json!({ "kind": "session_end", "role": "implementer", "ms": 60_000, "ok": true })),
            "implementer ended ok after 1m"
        );
        assert_eq!(
            d(json!({
                "kind": "session_end", "role": "scout", "scout": "spec", "ms": 60_000,
                "ok": false, "subtype": "error", "cost": 0.5, "error": "timed out",
            })),
            "scout spec ended error after 1m $0.50: timed out"
        );
    }

    #[test]
    fn step_start_marks_a_repeat_without_finishing() {
        assert_eq!(
            d(json!({ "kind": "step_start", "step": "checks", "key": "round 1" })),
            "checks round 1 started"
        );
        assert_eq!(
            d(json!({ "kind": "step_start", "step": "checks", "key": "round 1", "again": true })),
            "checks round 1 started AGAIN without finishing"
        );
    }

    #[test]
    fn step_end_without_checks_or_ok_falls_back_to_the_outcome_or_not_ok() {
        assert_eq!(
            d(json!({ "kind": "step_end", "step": "review", "key": "round 1", "ms": 60_000, "ok": true })),
            "review round 1 ended ok after 1m"
        );
        assert_eq!(
            d(json!({
                "kind": "step_end", "step": "review", "key": "round 1", "ms": 60_000,
                "ok": false, "outcome": "changes_requested",
            })),
            "review round 1 ended changes_requested after 1m"
        );
        assert_eq!(
            d(json!({ "kind": "step_end", "step": "review", "key": "round 1", "ms": 60_000, "ok": false })),
            "review round 1 ended not ok after 1m"
        );
    }

    #[test]
    fn ci_adds_how_long_it_took_only_when_given() {
        assert_eq!(d(json!({ "kind": "ci", "state": "passed" })), "CI passed");
        assert_eq!(
            d(json!({ "kind": "ci", "state": "passed", "ms": 60_000 })),
            "CI passed after 1m"
        );
    }

    #[test]
    fn merged_names_the_pr() {
        assert_eq!(d(json!({ "kind": "merged", "pr": 41 })), "merged PR #41");
    }

    #[test]
    fn error_names_where_and_what() {
        assert_eq!(
            d(json!({ "kind": "error", "where": "checks", "error": "timeout" })),
            "error in checks: timeout"
        );
    }

    #[test]
    fn store_error_adds_the_code_only_when_present() {
        assert_eq!(
            d(json!({ "kind": "store_error", "op": "claim", "error": "locked" })),
            "store claim failed: locked"
        );
        assert_eq!(
            d(json!({ "kind": "store_error", "op": "claim", "error": "locked", "code": "E_LOCK" })),
            "store claim failed (E_LOCK): locked"
        );
    }

    #[test]
    fn claim_names_the_answer() {
        assert_eq!(d(json!({ "kind": "claim", "answer": "yes" })), "claim answered: yes");
    }

    #[test]
    fn anomaly_marks_the_end_phase_differently_from_the_start() {
        assert_eq!(
            d(json!({ "kind": "anomaly", "phase": "start", "text": "stuck" })),
            "LOOKS WRONG: stuck"
        );
        assert_eq!(
            d(json!({ "kind": "anomaly", "phase": "end", "text": "stuck" })),
            "no longer: stuck"
        );
    }

    #[test]
    fn a_kind_without_a_line_of_its_own_and_an_unknown_one_show_their_other_fields_sorted_by_key() {
        assert_eq!(
            d(json!({ "kind": "mystery", "z": 1, "a": "x", "item": "i" })),
            "a=x z=1"
        );
        assert_eq!(
            d(json!({ "kind": "tick", "project": "p", "item": "i", "status": "running", "lanes": 2 })),
            "lanes=2 status=running"
        );
    }
}
