//! Reading the event log back (agent-telemetry.md section 5): `retaskable why -- <item>` and
//! `retaskable agent-report`. Pure over the events, apart from `read_all`, which reads the files.
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::events::{iso_utc_millis, parse_iso_ms, read_events};

/// A pause between an item's events worth a line of its own.
pub const GAP_SHOWN_MS: i64 = 10 * 60_000;
/// Every lane idle at least this long is an idle stretch in the report.
pub const IDLE_STRETCH_MS: i64 = 15 * 60_000;
/// Minute summaries further apart than this mean the agent wasn't running in between.
const TICK_GAP_MS: i64 = 3 * 60_000;

/// Every project's events under `data` (`.retaskable`), oldest first, each marked with its
/// project (a `"project"` field added to the event object).
pub fn read_all(data: &Path, since: i64, until: i64, project: Option<&str>) -> Vec<Value> {
    let root = data.join("projects");
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(p) = project
            && name != p
        {
            continue;
        }
        let dir = root.join(&name).join("events");
        for mut e in read_events(&dir, since, until) {
            if let Value::Object(map) = &mut e {
                map.insert("project".to_string(), Value::String(name.clone()));
            }
            out.push(e);
        }
    }
    out.sort_by(|a, b| {
        a.get("at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("at").and_then(Value::as_str).unwrap_or(""))
    });
    out
}

/// The flag of `retaskable why`, `retaskable agent-report` and `retaskable start-report`:
/// `--name value` or `--name=value`.
pub fn flag(args: &[String], name: &str) -> Option<String> {
    let dashed = format!("--{name}");
    let eq = format!("--{name}=");
    for (i, a) in args.iter().enumerate() {
        if *a == dashed {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = a.strip_prefix(eq.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

/// `retaskable why`'s one positional argument: the first value that is neither a flag nor a
/// flag's value.
pub fn find_item(args: &[String]) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if a.starts_with("--") {
            continue;
        }
        if i > 0 {
            let prev = &args[i - 1];
            if prev.starts_with("--") && !prev.contains('=') {
                continue;
            }
        }
        return Some(a.clone());
    }
    None
}

/// `--since`: an ISO time, or a span back from `now` (`"90m"`, `"6h"`, `"2d"`). `None` when it's
/// neither.
pub fn parse_since(text: &str, now: i64) -> Option<i64> {
    static DURATION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(\d+(?:\.\d+)?)\s*(m|min|h|d)$").expect("duration pattern"));
    let t = text.trim();
    if let Some(caps) = DURATION.captures(t) {
        let n: f64 = caps[1].parse().ok()?;
        let unit_ms = match caps[2].to_ascii_lowercase().as_str() {
            "m" | "min" => 60_000.0,
            "h" => 3_600_000.0,
            "d" => 86_400_000.0,
            _ => return None,
        };
        return Some(now - (n * unit_ms).round() as i64);
    }
    parse_iso_ms(t)
}

/// A duration, e.g. `90s`, `19m`, `4h 28m`, `2d 03h`.
pub fn span(ms: f64) -> String {
    let s = (ms.max(0.0) / 1000.0).round() as i64;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m");
    }
    let h = m / 60;
    if h < 48 {
        return format!("{h}h {:02}m", m % 60);
    }
    format!("{}d {}h", h / 24, h % 24)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_none_or(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

fn str_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

fn usd_of(v: Option<&Value>) -> String {
    match v.and_then(Value::as_f64) {
        Some(n) => format!("${n:.2}"),
        None => String::new(),
    }
}

fn ms_of(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).unwrap_or(0.0)
}

/// One event as a line's text, after its time.
pub fn describe(e: &Value) -> String {
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

struct AnomalySpan {
    rule: String,
    text: String,
    start: String,
    end: Option<String>,
}

/// Anomaly start and end events paired by id: each with how long it lasted, or open.
fn anomaly_spans(events: &[Value]) -> Vec<AnomalySpan> {
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

fn anomaly_line(a: &AnomalySpan, with_day: bool) -> String {
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
    use super::*;
    use serde_json::json;

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
    fn read_all_reads_every_projects_days_in_order_a_project_or_a_window_when_asked() {
        let f = fixture();
        let all = read_all(f.path(), 0, i64::MAX, None);
        let mut projects: Vec<String> = all.iter().map(|e| str_of(e.get("project"))).collect();
        projects.sort();
        projects.dedup();
        assert_eq!(projects, ["alpha", "beta"]);
        assert!(
            all.windows(2).all(|w| str_of(w[0].get("at")) <= str_of(w[1].get("at"))),
            "oldest first"
        );
        assert!(
            read_all(f.path(), 0, i64::MAX, Some("beta"))
                .iter()
                .all(|e| str_of(e.get("project")) == "beta")
        );
        let since60 = t0() + 60 * 60_000;
        assert!(
            read_all(f.path(), since60, i64::MAX, None)
                .iter()
                .all(|e| parse_iso_ms(&str_of(e.get("at"))).unwrap() >= since60)
        );
        assert_eq!(
            read_all(&f.path().join("nowhere"), 0, i64::MAX, None),
            Vec::<Value>::new()
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
}
