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
