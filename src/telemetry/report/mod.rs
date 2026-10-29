//! Reading the event log back (agent-telemetry.md section 5): `retaskable why -- <item>` and
//! `retaskable agent-report`. Pure over the events, apart from `read_all`, which reads the files.
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::telemetry::events::{parse_iso_ms, read_events};

mod agent_report;
mod describe;
#[cfg(test)]
mod tests;
mod why;

pub use agent_report::report_text;
pub use why::why_text;

/// A pause between an item's events worth a line of its own.
const GAP_SHOWN_MS: i64 = 10 * 60_000;
/// Every lane idle at least this long is an idle stretch in the report.
const IDLE_STRETCH_MS: i64 = 15 * 60_000;
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
fn span(ms: f64) -> String {
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
