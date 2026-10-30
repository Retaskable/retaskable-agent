//! `retaskable agent-report`: lane use, phase times, restarts and anomalies over a window.
use std::collections::HashMap;

use crate::telemetry::event::{Entry, Event, ItemState, RestartAsked, RestartExit, Tick};
use crate::telemetry::events::{iso_utc_millis, parse_iso_ms};

use super::why::{anomaly_line, anomaly_spans};
use super::{IDLE_STRETCH_MS, TICK_GAP_MS, span};

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
fn rework_why(note: &str, from: &str) -> String {
    let first = note.split([':', '(']).next().unwrap_or("").trim().to_string();
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

/// A minute summary and the time it was written.
type TickAt<'a> = (&'a Entry, &'a Tick);

fn lane_section(ticks: &[TickAt], to: i64) -> Vec<String> {
    if ticks.is_empty() {
        return vec!["  no minute summaries in this window".to_string()];
    }
    let lanes_of = |t: &Tick| t.lanes.unwrap_or(0);
    let max_lanes = ticks.iter().map(|(_, t)| lanes_of(t)).max().unwrap_or(0);
    let mut lines = Vec::new();
    let mut busy_sum = 0.0;
    let mut lane_sum = 0.0;
    for (_, t) in ticks {
        let lanes = lanes_of(t);
        let busy_count = t
            .busy
            .as_ref()
            .map(|busy| busy.iter().filter(|&&k| k <= lanes).count())
            .unwrap_or(0);
        busy_sum += busy_count as f64;
        lane_sum += lanes as f64;
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
        let counted: Vec<&Tick> = ticks.iter().map(|(_, t)| *t).filter(|t| k <= lanes_of(t)).collect();
        let busy = counted
            .iter()
            .filter(|t| t.busy.as_ref().is_some_and(|busy| busy.contains(&k)))
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
    let mut prev: Option<&Entry> = None;
    for (e, t) in ticks {
        let at = e.at.clone();
        if let Some(p) = prev {
            let p_at = p.at.clone();
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
        let busy_len = t.busy.as_ref().map(|b| b.len()).unwrap_or(0);
        let running = t.status.as_deref() == Some("running");
        let idle = running && busy_len == 0;
        if idle && idle_from.is_none() {
            idle_from = Some(at.clone());
        }
        if !idle {
            close_idle(&mut idle_from, &at, &mut stretches);
        }
        prev = Some(e);
    }
    if let Some(p) = prev {
        let p_ms = parse_iso_ms(&p.at).unwrap_or(0);
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

fn phase_section(events: &[Entry]) -> Vec<String> {
    let mut by_item: HashMap<String, Vec<(&Entry, &ItemState)>> = HashMap::new();
    for e in events {
        if let Some(Event::ItemState(s)) = e.event()
            && let Some(item) = &s.item
        {
            by_item.entry(item.clone()).or_default().push((e, s));
        }
    }
    let mut visits: HashMap<String, Vec<f64>> = HashMap::new();
    let mut slices: Vec<f64> = Vec::new();
    let mut rework: HashMap<String, i64> = HashMap::new();
    for moves in by_item.values() {
        let mut slice_start: Option<i64> = None;
        for (i, (e, s)) in moves.iter().enumerate() {
            let t = parse_iso_ms(&e.at).unwrap_or(0);
            let from = s.from.clone().unwrap_or_default();
            let to = s.to.clone().unwrap_or_default();
            if i > 0 {
                let (prev_e, prev_s) = moves[i - 1];
                if prev_s.to.as_deref().unwrap_or("") == from {
                    let prev_t = parse_iso_ms(&prev_e.at).unwrap_or(0);
                    visits.entry(from.clone()).or_default().push((t - prev_t) as f64);
                }
            }
            if to == "implementing" && !BACK_FROM.contains(&from.as_str()) && from != "implementing" {
                slice_start = Some(t);
            }
            if to == "implementing" && BACK_FROM.contains(&from.as_str()) {
                let why = rework_why(s.note.as_deref().unwrap_or(""), &from);
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

/// ` by @name` when the restart was asked by someone.
fn by_part(by: &Option<String>) -> String {
    match by.as_deref() {
        Some(b) if !b.is_empty() => format!(" by @{b}"),
        _ => String::new(),
    }
}

fn restart_section(events: &[Entry]) -> Vec<String> {
    // The machine's restarts are in every project's log: each once.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut once: Vec<&Entry> = Vec::new();
    for e in events {
        if !matches!(
            e.event(),
            Some(Event::RestartAsked(_) | Event::RestartExit(_) | Event::RestartBack(_))
        ) {
            continue;
        }
        let key = format!("{}{}", e.kind(), &e.at[..19.min(e.at.len())]);
        if seen.insert(key) {
            once.push(e);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut asked: Option<(&Entry, &RestartAsked)> = None;
    let mut exit: Option<&RestartExit> = None;
    for e in once {
        match e.event() {
            Some(Event::RestartAsked(a)) => asked = Some((e, a)),
            Some(Event::RestartExit(x)) => exit = Some(x),
            Some(Event::RestartBack(b)) => {
                let drained = exit
                    .map(|ex| span(ex.drain_ms.unwrap_or(0.0)))
                    .unwrap_or_else(|| "?".to_string());
                let down = b.down_ms.map(span).unwrap_or_else(|| "?".to_string());
                let at = &e.at;
                let prefix = match asked {
                    Some((a_e, a)) => format!(
                        "{} {} asked{} ({})",
                        &a_e.at[0..10],
                        &a_e.at[11..16],
                        by_part(&a.by),
                        a.reason.as_deref().unwrap_or("")
                    ),
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
    if let Some((a_e, a)) = asked {
        let tail = match exit {
            Some(ex) => format!(
                "exited after draining {}, not back in this window",
                span(ex.drain_ms.unwrap_or(0.0))
            ),
            None => "still waiting to restart".to_string(),
        };
        lines.push(format!(
            "  {} {} asked{}: {tail}",
            &a_e.at[0..10],
            &a_e.at[11..16],
            by_part(&a.by)
        ));
    }
    if lines.is_empty() {
        vec!["  none".to_string()]
    } else {
        lines
    }
}

/// `retaskable agent-report`: lane use, phase times, restarts and anomalies over `[from, to]`.
pub fn report_text(events: &[Entry], from: i64, to: i64) -> String {
    let in_window: Vec<Entry> = events
        .iter()
        .filter(|e| parse_iso_ms(&e.at).is_some_and(|t| t >= from && t <= to))
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
        let p = e.project.clone().unwrap_or_default();
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
        let mine: Vec<Entry> = in_window
            .iter()
            .filter(|e| e.project.as_deref().unwrap_or("") == project)
            .cloned()
            .collect();
        let ticks: Vec<TickAt> = mine
            .iter()
            .filter_map(|e| match e.event() {
                Some(Event::Tick(t)) => Some((e, t)),
                _ => None,
            })
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

    fn ev(v: serde_json::Value) -> Entry {
        Entry::from_value(&v).expect("an event")
    }

    /// The minute summaries among the entries.
    fn ticks(entries: &[Entry]) -> Vec<TickAt<'_>> {
        entries
            .iter()
            .filter_map(|e| match e.event() {
                Some(Event::Tick(t)) => Some((e, t)),
                _ => None,
            })
            .collect()
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
            rework_why("Blocking checks failed: test", "checking"),
            "Blocking checks failed"
        );
        assert_eq!(rework_why("Needs changes (see review)", "reviewing"), "Needs changes");
        assert_eq!(rework_why("", "checking"), "from checking");
        assert_eq!(rework_why("", "merging"), "from merging");
    }

    #[test]
    fn restart_section_is_none_without_any_restart_events() {
        assert_eq!(restart_section(&[]), vec!["  none".to_string()]);
    }

    #[test]
    fn restart_section_covers_still_waiting_and_exited_not_back() {
        let waiting = vec![ev(
            json!({ "at": at(0), "kind": "restart_asked", "reason": "asked", "by": "james" }),
        )];
        assert_eq!(
            restart_section(&waiting),
            vec!["  2026-10-15 08:00 asked by @james: still waiting to restart".to_string()]
        );

        let exited = vec![
            ev(json!({ "at": at(0), "kind": "restart_asked", "reason": "asked" })),
            ev(json!({ "at": at(1), "kind": "restart_exit", "reason": "asked", "drainMs": 45_000 })),
        ];
        assert_eq!(
            restart_section(&exited),
            vec!["  2026-10-15 08:00 asked: exited after draining 45s, not back in this window".to_string()]
        );
    }

    #[test]
    fn restart_section_dedupes_the_same_restart_logged_in_two_projects() {
        let twice = vec![
            ev(json!({ "at": at(0), "kind": "restart_asked", "reason": "asked", "by": "james", "project": "alpha" })),
            ev(json!({ "at": at(0), "kind": "restart_asked", "reason": "asked", "by": "james", "project": "beta" })),
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
        let entries = vec![
            ev(json!({ "at": at(0), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] })),
            ev(json!({ "at": at(1), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] })),
        ];
        assert!(
            lane_section(&ticks(&entries), t0() + 2 * 60_000)
                .contains(&"  no idle stretches of 15m or more".to_string())
        );
    }

    #[test]
    fn lane_section_marks_a_gap_between_summaries_as_the_agent_being_down() {
        let entries = vec![
            ev(json!({ "at": at(0), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] })),
            ev(json!({ "at": at(5), "kind": "tick", "status": "running", "lanes": 1, "busy": [1] })),
        ];
        assert!(
            lane_section(&ticks(&entries), t0() + 6 * 60_000)
                .contains(&"  down 2026-10-15 08:00 → 08:05 (5m): no summaries, the agent wasn't running".to_string())
        );
    }
}
