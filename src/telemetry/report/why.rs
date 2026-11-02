//! `retaskable why -- <item>`: one item's history, its anomalies, its sessions.
use std::collections::HashMap;

use crate::telemetry::event::{Entry, Event};
use crate::telemetry::events::parse_iso_ms;

use super::describe::describe;
use super::{GAP_SHOWN_MS, span};

pub(super) struct AnomalySpan {
    rule: String,
    text: String,
    start: String,
    end: Option<String>,
}

/// Anomaly start and end events paired by id: each with how long it lasted, or open.
pub(super) fn anomaly_spans(events: &[Entry]) -> Vec<AnomalySpan> {
    let mut out: Vec<AnomalySpan> = Vec::new();
    let mut open: HashMap<String, usize> = HashMap::new();
    for e in events {
        let Some(Event::Anomaly(a)) = e.event() else {
            continue;
        };
        let id = a.id.clone().unwrap_or_default();
        if a.phase.as_deref() == Some("start") {
            out.push(AnomalySpan {
                rule: a.rule.clone().unwrap_or_default(),
                text: a.text.clone().unwrap_or_default(),
                start: e.at.clone(),
                end: None,
            });
            open.insert(id, out.len() - 1);
        } else if let Some(idx) = open.remove(&id) {
            out[idx].end = Some(e.at.clone());
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
pub fn why_text(events: &[Entry], item: &str) -> String {
    let mine: Vec<Entry> = events.iter().filter(|e| e.item() == Some(item)).cloned().collect();
    if mine.is_empty() {
        return format!("Nothing in the event log for {item}.");
    }
    let mut projects: Vec<String> = Vec::new();
    for e in &mine {
        let p = e.project.clone().unwrap_or_default();
        if !projects.contains(&p) {
            projects.push(p);
        }
    }
    let first_at = mine[0].at.clone();
    let last_at = mine.last().unwrap().at.clone();
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
        let at = e.at.clone();
        let t = parse_iso_ms(&at).unwrap_or(0);
        if last != 0 && t - last >= GAP_SHOWN_MS {
            lines.push(format!("            … nothing for {}", span((t - last) as f64)));
        }
        let d = at[0..10].to_string();
        if d != last_day {
            last_day = d.clone();
            lines.push(last_day.clone());
        }
        lines.push(format!("  {}  {:<13} {}", &at[11..19], e.kind(), describe(e)));
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
    let sessions: Vec<(&Entry, &crate::telemetry::event::SessionEnd)> = mine
        .iter()
        .filter_map(|e| match e.event() {
            Some(Event::SessionEnd(s)) => Some((e, s)),
            _ => None,
        })
        .collect();
    lines.push(String::new());
    lines.push("Sessions".to_string());
    if sessions.is_empty() {
        lines.push("  none".to_string());
    } else {
        for (e, s) in sessions {
            let at = &e.at;
            let role = s.role.clone().unwrap_or_default();
            let role = match s.scout.as_deref() {
                Some(scout) if !scout.is_empty() => format!("{role} {scout}"),
                _ => role,
            };
            let outcome = if s.ok == Some(true) {
                "ok".to_string()
            } else {
                s.subtype.clone().unwrap_or_default()
            };
            lines.push(format!(
                "  {} {}  {role:<18} {:<7} {:>7}  {:>6}  {outcome}",
                &at[0..10],
                &at[11..16],
                s.model.as_deref().unwrap_or(""),
                span(s.ms.unwrap_or(0.0)),
                s.cost.map(|n| format!("${n:.2}")).unwrap_or_default(),
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "why_tests.rs"]
mod tests;
