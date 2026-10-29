//! `retaskable agent-report`: lane use, phase times, restarts and anomalies over a window.
use std::collections::HashMap;

use serde_json::Value;

use crate::telemetry::events::{iso_utc_millis, parse_iso_ms};

use super::why::{anomaly_line, anomaly_spans};
use super::{IDLE_STRETCH_MS, TICK_GAP_MS, ms_of, span, str_of, truthy};

fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    }
}

fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round() as i64)
}

/// The states a slice moves through, in the order the report shows them.
const PHASES: [&str; 7] = [
    "implementing",
    "checking",
    "reviewing",
    "awaiting_ci",
    "awaiting_playtest",
    "merging",
    "awaiting_merge",
];
const BACK_FROM: [&str; 6] = [
    "checking",
    "reviewing",
    "awaiting_ci",
    "awaiting_playtest",
    "merging",
    "awaiting_merge",
];

/// Why it went back to work, as a group: the note up to its first ":" or "(".
fn rework_why(note: Option<&Value>, from: &str) -> String {
    let text = str_of(note);
    let first = text.split([':', '(']).next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        format!("from {from}")
    } else {
        first
    }
}

fn close_idle(idle_from: &mut Option<String>, end_at: &str, stretches: &mut Vec<String>) {
    if let Some(from) = idle_from.take() {
        let dur = (parse_iso_ms(end_at).unwrap_or(0) - parse_iso_ms(&from).unwrap_or(0)) as f64;
        if dur >= IDLE_STRETCH_MS as f64 {
            stretches.push(format!(
                "  idle {} {} → {} ({}): every lane free",
                &from[0..10],
                &from[11..16],
                &end_at[11..16],
                span(dur)
            ));
        }
    }
}

fn lane_section(ticks: &[Value], to: i64) -> Vec<String> {
    if ticks.is_empty() {
        return vec!["  no minute summaries in this window".to_string()];
    }
    let max_lanes = ticks
        .iter()
        .filter_map(|t| t.get("lanes").and_then(Value::as_f64))
        .fold(0.0_f64, f64::max) as i64;
    let mut lines = Vec::new();
    let mut busy_sum = 0.0;
    let mut lane_sum = 0.0;
    for t in ticks {
        let lanes = t.get("lanes").and_then(Value::as_f64).unwrap_or(0.0);
        let busy_count = t
            .get("busy")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter(|k| k.as_f64().is_some_and(|n| n <= lanes)).count())
            .unwrap_or(0);
        busy_sum += busy_count as f64;
        lane_sum += lanes;
    }
    let n = ticks.len() as f64;
    lines.push(format!(
        "  overall: {:.2} of {:.2} lanes busy on average ({}), over {} minutes",
        busy_sum / n,
        lane_sum / n,
        if lane_sum > 0.0 {
            pct(busy_sum / lane_sum)
        } else {
            "0%".to_string()
        },
        ticks.len()
    ));
    for k in 1..=max_lanes {
        let counted: Vec<&Value> = ticks
            .iter()
            .filter(|t| k as f64 <= t.get("lanes").and_then(Value::as_f64).unwrap_or(0.0))
            .collect();
        let busy = counted
            .iter()
            .filter(|t| {
                t.get("busy")
                    .and_then(Value::as_array)
                    .is_some_and(|arr| arr.iter().any(|v| v.as_f64() == Some(k as f64)))
            })
            .count();
        lines.push(format!(
            "  lane {k}: busy {}",
            if counted.is_empty() {
                "0%".to_string()
            } else {
                pct(busy as f64 / counted.len() as f64)
            }
        ));
    }
    // Every lane idle, while running, for IDLE_STRETCH_MS or more; and stretches with no
    // summaries (the agent was down).
    let mut stretches: Vec<String> = Vec::new();
    let mut idle_from: Option<String> = None;
    let mut prev: Option<&Value> = None;
    for t in ticks {
        let at = str_of(t.get("at"));
        if let Some(p) = prev {
            let p_at = str_of(p.get("at"));
            let gap = parse_iso_ms(&at).unwrap_or(0) - parse_iso_ms(&p_at).unwrap_or(0);
            if gap > TICK_GAP_MS {
                close_idle(&mut idle_from, &p_at, &mut stretches);
                stretches.push(format!(
                    "  down {} {} → {} ({}): no summaries, the agent wasn't running",
                    &p_at[0..10],
                    &p_at[11..16],
                    &at[11..16],
                    span(gap as f64)
                ));
            }
        }
        let busy_len = t.get("busy").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
        let running = t.get("status").and_then(Value::as_str) == Some("running");
        let idle = running && busy_len == 0;
        if idle && idle_from.is_none() {
            idle_from = Some(at.clone());
        }
        if !idle {
            close_idle(&mut idle_from, &at, &mut stretches);
        }
        prev = Some(t);
    }
    if let Some(p) = prev {
        let p_ms = parse_iso_ms(&str_of(p.get("at"))).unwrap_or(0);
        let end_at = iso_utc_millis((p_ms + 60_000).min(to));
        close_idle(&mut idle_from, &end_at, &mut stretches);
    }
    if stretches.is_empty() {
        lines.push(format!(
            "  no idle stretches of {} or more",
            span(IDLE_STRETCH_MS as f64)
        ));
    } else {
        lines.extend(stretches);
    }
    lines
}

fn phase_section(events: &[Value]) -> Vec<String> {
    let mut by_item: HashMap<String, Vec<&Value>> = HashMap::new();
    for e in events {
        if e.get("kind").and_then(Value::as_str) == Some("item_state")
            && let Some(item) = e.get("item").and_then(Value::as_str)
        {
            by_item.entry(item.to_string()).or_default().push(e);
        }
    }
    let mut visits: HashMap<String, Vec<f64>> = HashMap::new();
    let mut slices: Vec<f64> = Vec::new();
    let mut rework: HashMap<String, i64> = HashMap::new();
    for moves in by_item.values() {
        let mut slice_start: Option<i64> = None;
        for (i, e) in moves.iter().enumerate() {
            let at = str_of(e.get("at"));
            let t = parse_iso_ms(&at).unwrap_or(0);
            let from = str_of(e.get("from"));
            let to = str_of(e.get("to"));
            if i > 0 {
                let prev = moves[i - 1];
                if str_of(prev.get("to")) == from {
                    let prev_t = parse_iso_ms(&str_of(prev.get("at"))).unwrap_or(0);
                    visits.entry(from.clone()).or_default().push((t - prev_t) as f64);
                }
            }
            if to == "implementing" && !BACK_FROM.contains(&from.as_str()) && from != "implementing" {
                slice_start = Some(t);
            }
            if to == "implementing" && BACK_FROM.contains(&from.as_str()) {
                let why = rework_why(e.get("note"), &from);
                *rework.entry(why).or_insert(0) += 1;
            }
            if (to == "merged" || to == "done") && slice_start.is_some() {
                slices.push((t - slice_start.unwrap()) as f64);
                slice_start = None;
            }
        }
    }
    let mut lines = vec![format!(
        "  slices merged: {}{}",
        slices.len(),
        if slices.is_empty() {
            String::new()
        } else {
            format!(", median {} from start to merge", span(median(&slices)))
        }
    )];
    for p in PHASES {
        if let Some(v) = visits.get(p)
            && !v.is_empty()
        {
            lines.push(format!(
                "  {:<18} median {:>7}  ({} time{})",
                p,
                span(median(v)),
                v.len(),
                if v.len() == 1 { "" } else { "s" }
            ));
        }
    }
    let mut rounds: Vec<(String, i64)> = rework.into_iter().collect();
    rounds.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    lines.push(format!(
        "  rework rounds: {}",
        rounds.iter().map(|(_, c)| c).sum::<i64>()
    ));
    for (why, n) in &rounds {
        lines.push(format!("    {n:>3}  {why}"));
    }
    lines
}

fn restart_section(events: &[Value]) -> Vec<String> {
    // The machine's restarts are in every project's log: each once.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut once: Vec<&Value> = Vec::new();
    for e in events {
        let kind = str_of(e.get("kind"));
        if !kind.starts_with("restart_") || kind == "restart_waiting" {
            continue;
        }
        let at = str_of(e.get("at"));
        let key = format!("{kind}{}", &at[..19.min(at.len())]);
        if seen.insert(key) {
            once.push(e);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut asked: Option<&Value> = None;
    let mut exit: Option<&Value> = None;
    for e in once {
        match str_of(e.get("kind")).as_str() {
            "restart_asked" => asked = Some(e),
            "restart_exit" => exit = Some(e),
            "restart_back" => {
                let drained = exit
                    .map(|ex| span(ms_of(ex.get("drainMs"))))
                    .unwrap_or_else(|| "?".to_string());
                let down = e
                    .get("downMs")
                    .and_then(Value::as_f64)
                    .map(span)
                    .unwrap_or_else(|| "?".to_string());
                let at = str_of(e.get("at"));
                let prefix = match asked {
                    Some(a) => {
                        let a_at = str_of(a.get("at"));
                        let by = if truthy(a.get("by")) {
                            format!(" by @{}", str_of(a.get("by")))
                        } else {
                            String::new()
                        };
                        format!(
                            "{} {} asked{by} ({})",
                            &a_at[0..10],
                            &a_at[11..16],
                            str_of(a.get("reason"))
                        )
                    }
                    None => format!("{} back", &at[0..10]),
                };
                lines.push(format!(
                    "  {prefix}: drained {drained}, down {down}, back at {}",
                    &at[11..16]
                ));
                asked = None;
                exit = None;
            }
            _ => {}
        }
    }
    if let Some(a) = asked {
        let a_at = str_of(a.get("at"));
        let by = if truthy(a.get("by")) {
            format!(" by @{}", str_of(a.get("by")))
        } else {
            String::new()
        };
        let tail = match exit {
            Some(ex) => format!(
                "exited after draining {}, not back in this window",
                span(ms_of(ex.get("drainMs")))
            ),
            None => "still waiting to restart".to_string(),
        };
        lines.push(format!("  {} {} asked{by}: {tail}", &a_at[0..10], &a_at[11..16]));
    }
    if lines.is_empty() {
        vec!["  none".to_string()]
    } else {
        lines
    }
}

/// `retaskable agent-report`: lane use, phase times, restarts and anomalies over `[from, to]`.
pub fn report_text(events: &[Value], from: i64, to: i64) -> String {
    let in_window: Vec<Value> = events
        .iter()
        .filter(|e| {
            let t = e.get("at").and_then(Value::as_str).and_then(parse_iso_ms);
            t.is_some_and(|t| t >= from && t <= to)
        })
        .cloned()
        .collect();
    let from_iso = iso_utc_millis(from);
    let to_iso = iso_utc_millis(to);
    let header = format!(
        "Agent report, {} {} to {} {} UTC ({})",
        &from_iso[0..10],
        &from_iso[11..16],
        &to_iso[0..10],
        &to_iso[11..16],
        span((to - from) as f64)
    );
    let mut projects: Vec<String> = Vec::new();
    for e in &in_window {
        let p = str_of(e.get("project"));
        if !projects.contains(&p) {
            projects.push(p);
        }
    }
    if projects.is_empty() {
        return format!("{header}\n\nNo events in this window.");
    }
    projects.sort();
    let mut lines = vec![header];
    for project in &projects {
        let mine: Vec<Value> = in_window
            .iter()
            .filter(|e| str_of(e.get("project")) == *project)
            .cloned()
            .collect();
        let ticks: Vec<Value> = mine
            .iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) == Some("tick"))
            .cloned()
            .collect();
        lines.push(String::new());
        lines.push(format!("== {project}"));
        lines.push(String::new());
        lines.push("Lanes".to_string());
        lines.extend(lane_section(&ticks, to));
        lines.push(String::new());
        lines.push("Phases (per visit to each state)".to_string());
        lines.extend(phase_section(&mine));
        let anomalies = anomaly_spans(&mine);
        lines.push(String::new());
        lines.push("Anomalies".to_string());
        if anomalies.is_empty() {
            lines.push("  none".to_string());
        } else {
            lines.extend(anomalies.iter().map(|a| anomaly_line(a, true)));
        }
    }
    lines.push(String::new());
    lines.push("Restarts".to_string());
    lines.extend(restart_section(&in_window));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const T0_TEXT: &str = "2026-10-15T08:00:00.000Z";

    fn t0() -> i64 {
        parse_iso_ms(T0_TEXT).unwrap()
    }

    fn at(min: i64) -> String {
        iso_utc_millis(t0() + min * 60_000)
    }

    #[test]
    fn median_is_nan_when_empty_and_averages_the_middle_pair_when_even() {
        assert!(median(&[]).is_nan());
        assert_eq!(
            median(&[3.0, 1.0, 2.0]),
            2.0,
            "odd count: the middle value, once sorted"
        );
        assert_eq!(
            median(&[1.0, 2.0, 3.0, 4.0]),
            2.5,
            "even count: the middle pair, averaged"
        );
    }

    #[test]
    fn pct_rounds_to_the_nearest_whole_percent() {
        assert_eq!(pct(0.0), "0%");
        assert_eq!(pct(2.0 / 3.0), "67%");
        assert_eq!(pct(1.0), "100%");
    }

    #[test]
    fn rework_why_groups_by_the_notes_prefix_and_falls_back_to_from() {
        assert_eq!(
            rework_why(Some(&json!("Blocking checks failed: test")), "checking"),
            "Blocking checks failed"
        );
        assert_eq!(
            rework_why(Some(&json!("Needs changes (see review)")), "reviewing"),
            "Needs changes"
        );
        assert_eq!(rework_why(None, "checking"), "from checking");
        assert_eq!(rework_why(Some(&json!("")), "merging"), "from merging");
    }

    #[test]
    fn restart_section_is_none_without_any_restart_events() {
        assert_eq!(restart_section(&[]), vec!["  none".to_string()]);
    }

    #[test]
    fn restart_section_covers_still_waiting_and_exited_not_back() {
        let waiting = vec![json!({ "at": at(0), "kind": "restart_asked", "reason": "asked", "by": "james" })];
        assert_eq!(
            restart_section(&waiting),
            vec!["  2026-10-15 08:00 asked by @james: still waiting to restart".to_string()]
        );

        let exited = vec![
            json!({ "at": at(0), "kind": "restart_asked", "reason": "asked" }),
            json!({ "at": at(1), "kind": "restart_exit", "reason": "asked", "drainMs": 45_000 }),
        ];
        assert_eq!(
            restart_section(&exited),
            vec!["  2026-10-15 08:00 asked: exited after draining 45s, not back in this window".to_string()]
        );
    }

    #[test]
    fn restart_section_dedupes_the_same_restart_logged_in_two_projects() {
        let twice = vec![
            json!({ "at": at(0), "kind": "restart_asked", "reason": "asked", "by": "james", "project": "alpha" }),
            json!({ "at": at(0), "kind": "restart_asked", "reason": "asked", "by": "james", "project": "beta" }),
        ];
        assert_eq!(
            restart_section(&twice),
            vec!["  2026-10-15 08:00 asked by @james: still waiting to restart".to_string()]
        );
    }

    #[test]
    fn lane_section_says_so_without_any_minute_summaries() {
        assert_eq!(
            lane_section(&[], t0()),
            vec!["  no minute summaries in this window".to_string()]
        );
    }

    #[test]
    fn lane_section_has_no_idle_stretches_when_a_lane_is_always_busy() {
        let ticks = vec![
            json!({ "at": at(0), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] }),
            json!({ "at": at(1), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] }),
        ];
        assert!(lane_section(&ticks, t0() + 2 * 60_000).contains(&"  no idle stretches of 15m or more".to_string()));
    }

    #[test]
    fn lane_section_marks_a_gap_between_summaries_as_the_agent_being_down() {
        let ticks = vec![
            json!({ "at": at(0), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] }),
            json!({ "at": at(5), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] }),
        ];
        assert!(
            lane_section(&ticks, t0() + 6 * 60_000)
                .contains(&"  down 2026-10-15 08:00 → 08:05 (5m): no summaries, the agent wasn't running".to_string())
        );
    }
}
