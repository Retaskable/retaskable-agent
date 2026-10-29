//! `retaskable why -- <item>`: one item's history, its anomalies, its sessions.
use std::collections::HashMap;

use serde_json::Value;

use crate::telemetry::events::parse_iso_ms;

use super::describe::describe;
use super::{GAP_SHOWN_MS, ms_of, span, str_of, truthy, usd_of};

pub(super) struct AnomalySpan {
    rule: String,
    text: String,
    start: String,
    end: Option<String>,
}

/// Anomaly start and end events paired by id: each with how long it lasted, or open.
pub(super) fn anomaly_spans(events: &[Value]) -> Vec<AnomalySpan> {
    let mut out: Vec<AnomalySpan> = Vec::new();
    let mut open: HashMap<String, usize> = HashMap::new();
    for e in events {
        if e.get("kind").and_then(Value::as_str) != Some("anomaly") {
            continue;
        }
        let id = str_of(e.get("id"));
        if e.get("phase").and_then(Value::as_str) == Some("start") {
            out.push(AnomalySpan {
                rule: str_of(e.get("rule")),
                text: str_of(e.get("text")),
                start: str_of(e.get("at")),
                end: None,
            });
            open.insert(id, out.len() - 1);
        } else if let Some(idx) = open.remove(&id) {
            out[idx].end = Some(str_of(e.get("at")));
        }
    }
    out
}

pub(super) fn anomaly_line(a: &AnomalySpan, with_day: bool) -> String {
    let from = if with_day {
        format!("{} {}", &a.start[0..10], &a.start[11..16])
    } else {
        a.start[11..16].to_string()
    };
    let until = match &a.end {
        Some(end) => {
            let dur = (parse_iso_ms(end).unwrap_or(0) - parse_iso_ms(&a.start).unwrap_or(0)) as f64;
            format!("{} ({})", &end[11..16], span(dur))
        }
        None => "still open".to_string(),
    };
    format!("  {from} → {until}  {}: {}", a.rule, a.text)
}

/// `retaskable why -- <item>`: the item's events in order with long gaps marked, its anomalies
/// and its sessions.
pub fn why_text(events: &[Value], item: &str) -> String {
    let mine: Vec<Value> = events
        .iter()
        .filter(|e| e.get("item").and_then(Value::as_str) == Some(item))
        .cloned()
        .collect();
    if mine.is_empty() {
        return format!("Nothing in the event log for {item}.");
    }
    let mut projects: Vec<String> = Vec::new();
    for e in &mine {
        let p = str_of(e.get("project"));
        if !projects.contains(&p) {
            projects.push(p);
        }
    }
    let first_at = str_of(mine[0].get("at"));
    let last_at = str_of(mine.last().unwrap().get("at"));
    let mut lines = vec![
        format!(
            "{item} ({}): {} events, {} {} to {} {} UTC",
            projects.join(", "),
            mine.len(),
            &first_at[0..10],
            &first_at[11..19],
            &last_at[0..10],
            &last_at[11..19]
        ),
        String::new(),
    ];
    let mut last_day = String::new();
    let mut last: i64 = 0;
    for e in &mine {
        let at = str_of(e.get("at"));
        let t = parse_iso_ms(&at).unwrap_or(0);
        if last != 0 && t - last >= GAP_SHOWN_MS {
            lines.push(format!("            … nothing for {}", span((t - last) as f64)));
        }
        let d = at[0..10].to_string();
        if d != last_day {
            last_day = d.clone();
            lines.push(last_day.clone());
        }
        lines.push(format!(
            "  {}  {:<13} {}",
            &at[11..19],
            str_of(e.get("kind")),
            describe(e)
        ));
        last = t;
    }
    let anomalies = anomaly_spans(&mine);
    lines.push(String::new());
    lines.push("Anomalies".to_string());
    if anomalies.is_empty() {
        lines.push("  none".to_string());
    } else {
        lines.extend(anomalies.iter().map(|a| anomaly_line(a, true)));
    }
    let sessions: Vec<&Value> = mine
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some("session_end"))
        .collect();
    lines.push(String::new());
    lines.push("Sessions".to_string());
    if sessions.is_empty() {
        lines.push("  none".to_string());
    } else {
        for e in sessions {
            let at = str_of(e.get("at"));
            let role = if truthy(e.get("scout")) {
                format!("{} {}", str_of(e.get("role")), str_of(e.get("scout")))
            } else {
                str_of(e.get("role"))
            };
            let outcome = if truthy(e.get("ok")) {
                "ok".to_string()
            } else {
                str_of(e.get("subtype"))
            };
            lines.push(format!(
                "  {} {}  {role:<18} {:<7} {:>7}  {:>6}  {outcome}",
                &at[0..10],
                &at[11..16],
                str_of(e.get("model")),
                span(ms_of(e.get("ms"))),
                usd_of(e.get("cost")),
            ));
        }
    }
    lines.join("\n")
}
