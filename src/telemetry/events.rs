//! The agent's event log (agent-telemetry.md section 3): per project, one JSON line per event in
//! `.retaskable/projects/<id>/events/<YYYY-MM-DD>.jsonl` (UTC days), appended, kept 14 days. Ids,
//! states, durations, counts and short texts only: never a prompt, a transcript line or a secret.
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::event::{Entry, Event};

/// Days of files kept.
pub const KEEP_DAYS: i64 = 14;
/// A note or an error is cut to this many characters.
pub const MAX_TEXT: usize = 300;
/// Events kept in memory for the watchdog, and read back from the files when the log opens.
pub const RECENT_MS: i64 = 24 * 60 * 60_000;
const RECENT_MAX: usize = 20_000;
const DAY_MS: i64 = 24 * 60 * 60_000;

/// Fields never written, whatever they hold: what a session was told or said (and a lease blob).
const DROPPED: &[&str] = &["prompt", "followUp", "transcript", "message", "body", "token", "lease"];

static TOKEN_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,}|sk-[A-Za-z0-9_-]{16,}|xox[abprs]-[A-Za-z0-9-]{8,}|glpat-[A-Za-z0-9_-]{16,})")
        .expect("token secret pattern")
});
static BEARER_SECRET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}").expect("bearer secret pattern"));
static KEY_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b((?:access_?|refresh_?)?token|secret|password|passwd|api[_-]?key|authorization)(["']?\s*[:=]\s*["']?)[^\s"',;&]+"#)
        .expect("key secret pattern")
});
static DAY_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})\.jsonl$").expect("day name"));
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("whitespace"));

/// A text fit for the log: secrets replaced, one line's worth, at most `MAX_TEXT` characters.
pub fn scrub(text: &str, max: usize) -> String {
    let mut t = TOKEN_SECRET.replace_all(text, "[redacted]").into_owned();
    t = BEARER_SECRET
        .replace_all(&t, |caps: &regex::Captures| format!("{} [redacted]", &caps[1]))
        .into_owned();
    t = KEY_SECRET
        .replace_all(&t, |caps: &regex::Captures| {
            format!("{}{}[redacted]", &caps[1], &caps[2])
        })
        .into_owned();
    t = WHITESPACE.replace_all(&t, " ").trim().to_string();
    if t.len() > max {
        let keep = max.saturating_sub(1);
        // Cut on a char boundary.
        let mut end = keep;
        while end > 0 && !t.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &t[..end])
    } else {
        t
    }
}

/// Every string scrubbed, the dropped fields left out, missing fields left out.
pub fn clean(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(s) => Value::String(scrub(s, MAX_TEXT)),
        Value::Array(arr) => Value::Array(arr.iter().take(50).map(|v| clean(v, depth + 1)).collect()),
        Value::Object(obj) => {
            if depth > 3 {
                return Value::Null;
            }
            let mut out = Map::new();
            for (k, v) in obj {
                if DROPPED.contains(&k.as_str()) {
                    continue;
                }
                out.insert(k.clone(), clean(v, depth + 1));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// A UTC day's file path under `dir`.
pub fn day_file(dir: &Path, at_ms: i64) -> PathBuf {
    dir.join(format!("{}.jsonl", iso_utc_day(at_ms)))
}

/// Deletes the day files older than `keep_days` (by their names). Answers the names deleted.
pub fn prune(dir: &Path, now_ms: i64, keep_days: i64) -> Vec<String> {
    let names = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let oldest = iso_utc_day(now_ms - (keep_days - 1) * DAY_MS);
    let mut gone = Vec::new();
    for entry in names.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(caps) = DAY_NAME.captures(&name) else {
            continue;
        };
        let day = &caps[1];
        if day >= oldest.as_str() {
            continue;
        }
        match std::fs::remove_file(entry.path()) {
            Ok(()) => gone.push(name.into_owned()),
            Err(_) => {
                // Held by something else: next time.
            }
        }
    }
    gone
}

/// The events in `dir` from `since` to `until` (epoch ms), in file order. Lines that don't parse are skipped.
pub fn read_events(dir: &Path, since: i64, until: i64) -> Vec<Entry> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                DAY_NAME.is_match(&name).then_some(name)
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    names.sort();
    let first = if since > 0 { iso_utc_day(since) } else { String::new() };
    // Match TypeScript's `until = Infinity` → last day name "9999".
    let last = if until >= i64::MAX / 2 {
        "9999".to_string()
    } else {
        iso_utc_day(until)
    };
    let mut out = Vec::new();
    for name in names {
        let day = &name[..10.min(name.len())];
        if day < first.as_str() || day > last.as_str() {
            continue;
        }
        let text = match std::fs::read_to_string(dir.join(&name)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in text.split(['\n', '\r']) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue; // A line cut short by a crash.
            };
            let Some(e) = Entry::from_value(&v) else {
                continue; // Not an event: no kind, or no time.
            };
            let Some(t) = parse_iso_ms(&e.at) else {
                continue;
            };
            if t >= since && t <= until {
                out.push(e);
            }
        }
    }
    out
}

/// Callback for every event (including those read back when the log opens).
type EventListener = Box<dyn FnMut(&Entry) + Send>;

/// One project's log: events kept in memory for the watchdog, and appended to the day's file once it's open.
pub struct EventLog {
    /// The last `RECENT_MS` of events, oldest first.
    recent: Vec<Entry>,
    dir: Option<PathBuf>,
    pruned_day: String,
    listeners: Vec<EventListener>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl EventLog {
    pub fn new() -> Self {
        Self::with_now(now_ms)
    }

    pub fn with_now(now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self {
            recent: Vec::new(),
            dir: None,
            pruned_day: String::new(),
            listeners: Vec::new(),
            now: Box::new(now),
        }
    }

    /// Starts writing to `dir`: old files are pruned and the last day is read back.
    pub fn open(&mut self, dir: &Path) {
        if self.dir.as_deref() == Some(dir) {
            return;
        }
        self.dir = Some(dir.to_path_buf());
        let _ = std::fs::create_dir_all(dir);
        self.prune_daily();
        let since = (self.now)() - RECENT_MS;
        let known: std::collections::HashSet<String> = self
            .recent
            .iter()
            .filter_map(|e| serde_json::to_string(&e.to_value()).ok())
            .collect();
        let back: Vec<Entry> = read_events(dir, since, i64::MAX)
            .into_iter()
            .filter(|e| {
                serde_json::to_string(&e.to_value())
                    .map(|s| !known.contains(&s))
                    .unwrap_or(true)
            })
            .collect();
        for e in &back {
            for f in &mut self.listeners {
                f(e);
            }
        }
        let mut merged = back;
        merged.append(&mut self.recent);
        let start = merged.len().saturating_sub(RECENT_MAX);
        self.recent = merged.split_off(start);
    }

    pub fn writing(&self) -> bool {
        self.dir.is_some()
    }

    /// Called with every event, those read back on open included.
    pub fn on_event(&mut self, f: impl FnMut(&Entry) + Send + 'static) {
        self.listeners.push(Box::new(f));
    }

    pub fn recent(&self) -> &[Entry] {
        &self.recent
    }

    /// Logs `event` now, with no project of its own (the file it goes to is the project's).
    pub fn add(&mut self, event: Event) -> Entry {
        self.push(None, event)
    }

    /// Logs `event` now, marked with the project it is about.
    pub fn add_in(&mut self, project: &str, event: Event) -> Entry {
        self.push(Some(project), event)
    }

    fn push(&mut self, project: Option<&str>, event: Event) -> Entry {
        let now = (self.now)();
        // Cleaned as JSON, then read back as an entry: a field cleaned into the wrong shape leaves
        // the entry as an `Other`, still cleaned.
        let mut cleaned = Entry::new(iso_utc_millis(now), project.map(str::to_string), event).to_value();
        cleaned = clean(&cleaned, 0);
        let event = Entry::from_value(&cleaned).expect("a logged event has a kind and a time");
        self.recent.push(event.clone());
        let mut drop = 0;
        while drop < self.recent.len() {
            let at = parse_iso_ms(&self.recent[drop].at).unwrap_or(0);
            if at >= now - RECENT_MS {
                break;
            }
            drop += 1;
        }
        if self.recent.len() - drop > RECENT_MAX {
            drop = self.recent.len() - RECENT_MAX;
        }
        if drop > 0 {
            self.recent.drain(0..drop);
        }
        for f in &mut self.listeners {
            f(&event);
        }
        if let Some(dir) = self.dir.clone() {
            self.prune_daily();
            let path = day_file(&dir, now);
            let line = match serde_json::to_string(&event.to_value()) {
                Ok(s) => s + "\n",
                Err(_) => return event,
            };
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(line.as_bytes())
                });
            // A write that fails is dropped silently: the log must never stop the agent.
        }
        event
    }

    fn prune_daily(&mut self) {
        let day = iso_utc_day((self.now)());
        let Some(dir) = self.dir.clone() else {
            return;
        };
        if day == self.pruned_day {
            return;
        }
        self.pruned_day = day;
        prune(&dir, (self.now)(), KEEP_DAYS);
    }
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}

/// `YYYY-MM-DD` for a UTC instant in epoch milliseconds.
fn iso_utc_day(ms: i64) -> String {
    let (y, m, d, _, _, _) = civil_from_ms(ms);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Full ISO-8601 UTC with milliseconds, e.g. `2026-10-15T23:59:30.000Z`.
pub(crate) fn iso_utc_millis(ms: i64) -> String {
    let (y, m, d, hh, mm, ss) = civil_from_ms(ms);
    let millis = ms.rem_euclid(1000) as u32;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Now, in epoch milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn parse_iso_ms(s: &str) -> Option<i64> {
    // Accept `YYYY-MM-DDTHH:MM:SS.sssZ` (and without fractional seconds).
    let s = s.strip_suffix('Z').unwrap_or(s);
    let (date, time) = s.split_once('T')?;
    let mut dparts = date.split('-');
    let y: i64 = dparts.next()?.parse().ok()?;
    let mo: u32 = dparts.next()?.parse().ok()?;
    let d: u32 = dparts.next()?.parse().ok()?;
    let (hms, frac) = match time.split_once('.') {
        Some((hms, frac)) => (hms, Some(frac)),
        None => (time, None),
    };
    let mut tparts = hms.split(':');
    let hh: u32 = tparts.next()?.parse().ok()?;
    let mm: u32 = tparts.next()?.parse().ok()?;
    let ss: u32 = tparts.next()?.parse().ok()?;
    let mut millis: i64 = 0;
    if let Some(frac) = frac {
        let digits: String = frac.chars().take(3).collect();
        let padded = format!("{digits:0<3}");
        millis = padded.parse().ok()?;
    }
    Some(ms_from_civil(y, mo, d, hh, mm, ss) + millis)
}

/// Civil UTC components from epoch milliseconds (Howard Hinnant's `civil_from_days`).
fn civil_from_ms(ms: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let ss = tod % 60;
    // days since 1970-01-01 → civil
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = era * 400 + yoe as i64;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hh, mm, ss)
}

fn ms_from_civil(y: i64, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> i64 {
    // Inverse of civil_from_days.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    days * 86_400_000 + (hh as i64) * 3_600_000 + (mm as i64) * 60_000 + (ss as i64) * 1_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::event::{Body, ItemState, Lease, Tick};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn t0() -> i64 {
        parse_iso_ms("2026-10-15T23:59:30.000Z").unwrap()
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn one_json_line_per_event_appended_to_the_utc_days_file() {
        let dir = tmp();
        let t0 = t0();
        let now = Arc::new(Mutex::new(t0));
        let now_c = Arc::clone(&now);
        let mut log = EventLog::with_now(move || *now_c.lock().unwrap());
        log.add(Event::Lease(Lease {
            item: Some("R-1".into()),
            ..Default::default()
        }));
        log.open(dir.path());
        log.add(Event::ItemState(ItemState {
            item: Some("R-1".into()),
            from: Some("queued".into()),
            to: Some("implementing".into()),
            lane: Some(2),
            ..Default::default()
        }));
        *now.lock().unwrap() += 60_000; // past midnight UTC: the next day's file
        log.add(Event::Tick(Tick {
            lanes: Some(4),
            busy: Some(vec![1, 2]),
            sessions: Some(json!(2)),
            ..Default::default()
        }));
        let day1 = std::fs::read_to_string(day_file(dir.path(), t0)).unwrap();
        let day1: Vec<&str> = day1.trim().split(['\n', '\r']).filter(|l| !l.is_empty()).collect();
        assert_eq!(day1.len(), 1, "nothing is written before the log opens");
        assert_eq!(
            serde_json::from_str::<Value>(day1[0]).unwrap(),
            json!({
                "at": "2026-10-15T23:59:30.000Z",
                "kind": "item_state",
                "item": "R-1",
                "from": "queued",
                "to": "implementing",
                "lane": 2
            })
        );
        let now_ms = *now.lock().unwrap();
        assert_eq!(day_file(dir.path(), now_ms).file_name().unwrap(), "2026-10-16.jsonl");
        let day2: Value =
            serde_json::from_str(&std::fs::read_to_string(day_file(dir.path(), now_ms)).unwrap()).unwrap();
        assert_eq!(
            day2,
            json!({
                "at": "2026-10-16T00:00:30.000Z",
                "kind": "tick",
                "lanes": 4,
                "busy": [1, 2],
                "sessions": 2
            })
        );
        let kinds: Vec<_> = log.recent().iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds,
            ["lease", "item_state", "tick"],
            "all kept in memory for the watchdog"
        );
    }

    #[test]
    fn no_secrets_no_prompts_tokens_redacted_long_texts_cut_session_words_dropped() {
        assert_eq!(
            scrub("gh failed: token ghp_abcdefghijklmnopqrstuvwxyz0123 refused", MAX_TEXT),
            "gh failed: token [redacted] refused"
        );
        assert_eq!(
            scrub("Authorization: Bearer abc.def.ghi-jkl", MAX_TEXT),
            "Authorization: [redacted] [redacted]"
        );
        assert_eq!(
            scrub(
                "curl -H 'x' password=hunter22 api_key: sk-ant-api03-abcdefghijklmnopqrstu",
                MAX_TEXT
            ),
            "curl -H 'x' password=[redacted] api_key: [redacted]"
        );
        assert_eq!(
            scrub(
                "github_pat_11ABCDEFG0123456789_abcdefghijkl and xoxb-1234-5678-abcdefgh",
                MAX_TEXT
            ),
            "[redacted] and [redacted]"
        );
        let long = scrub(&"x".repeat(1000), MAX_TEXT);
        assert_eq!(long.chars().count(), MAX_TEXT);
        assert!(long.ends_with('…'));
        assert_eq!(scrub("two\n  lines", MAX_TEXT), "two lines");
        let cleaned = clean(
            &json!({
                "item": "R-1",
                "prompt": "Build the thing",
                "followUp": "Answers…",
                "transcript": "…",
                "message": "m",
                "token": "t",
                "lease": "l",
                "note": "Merged PR #4",
                "nested": { "body": "b", "ok": true },
            }),
            0,
        );
        assert_eq!(
            cleaned,
            json!({ "item": "R-1", "note": "Merged PR #4", "nested": { "ok": true } })
        );
    }

    #[test]
    fn files_past_14_days_are_pruned_when_the_log_opens_and_only_day_files_are_touched() {
        let dir = tmp();
        let t0 = t0();
        let day = |offset: i64| format!("{}.jsonl", iso_utc_day(t0 - offset * 86_400_000));
        for d in [0, 1, KEEP_DAYS - 1, KEEP_DAYS, KEEP_DAYS + 5] {
            std::fs::write(dir.path().join(day(d)), "").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "keep").unwrap();
        let mut gone = prune(dir.path(), t0, KEEP_DAYS);
        gone.sort();
        let mut expected = vec![day(KEEP_DAYS + 5), day(KEEP_DAYS)];
        expected.sort();
        assert_eq!(gone, expected);
        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut want = vec![day(KEEP_DAYS - 1), day(1), day(0), "notes.txt".into()];
        want.sort();
        assert_eq!(left, want);
        std::fs::write(dir.path().join(day(30)), "").unwrap();
        EventLog::with_now(move || t0).open(dir.path());
        assert!(!dir.path().join(day(30)).exists(), "opening prunes");
    }

    #[test]
    fn reads_a_window_back_skipping_broken_lines_opening_reads_the_last_day_back() {
        let dir = tmp();
        let line = |at: &str, kind: &str| format!("{}\n", json!({ "at": at, "kind": kind }));
        std::fs::write(
            dir.path().join("2026-10-14.jsonl"),
            format!(
                "{}{}",
                line("2026-10-14T10:00:00.000Z", "old"),
                line("2026-10-14T23:00:00.000Z", "late")
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("2026-10-15.jsonl"),
            format!(
                "{}{{not json\n{}",
                line("2026-10-15T08:00:00.000Z", "a"),
                line("2026-10-15T09:00:00.000Z", "b")
            ),
        )
        .unwrap();
        let kinds: Vec<_> = read_events(dir.path(), 0, i64::MAX).iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, ["old", "late", "a", "b"]);
        let window: Vec<_> = read_events(
            dir.path(),
            parse_iso_ms("2026-10-14T12:00:00Z").unwrap(),
            parse_iso_ms("2026-10-15T08:30:00Z").unwrap(),
        )
        .iter()
        .map(|e| e.kind())
        .collect();
        assert_eq!(window, ["late", "a"]);

        let heard = Arc::new(Mutex::new(Vec::new()));
        let heard_c = Arc::clone(&heard);
        let mut log = EventLog::with_now(|| parse_iso_ms("2026-10-15T12:00:00Z").unwrap());
        log.on_event(move |e| {
            heard_c.lock().unwrap().push(e.kind());
        });
        log.open(dir.path());
        let kinds: Vec<_> = log.recent().iter().map(|e| e.kind()).collect();
        assert_eq!(kinds, ["late", "a", "b"], "the last 24 hours");
        assert_eq!(
            *heard.lock().unwrap(),
            ["late", "a", "b"],
            "listeners hear what's read back too"
        );
    }

    /// Lines as the agent writes them (the TypeScript agent's and this one's), one per kind.
    fn real_lines() -> Vec<Value> {
        let at = "2026-10-15T08:00:00.000Z";
        vec![
            json!({ "at": at, "kind": "tick", "status": "running", "lanes": 2, "busy": [1], "free": [2], "sessions": 1, "states": { "implementing": 1 }, "claim": "idle" }),
            json!({ "at": at, "kind": "item_state", "item": "R-12", "from": "queued", "to": "implementing", "note": "Slice 1 started", "lane": 1 }),
            json!({ "at": at, "kind": "lease", "item": "R-12", "change": "renewed", "state": "held" }),
            json!({ "at": at, "kind": "lane_taken", "item": "R-12", "lane": 1, "why": "implementing" }),
            json!({ "at": at, "kind": "lane_freed", "item": "R-12", "lane": 1, "why": "merged" }),
            json!({ "at": at, "kind": "session_start", "item": "R-12", "lane": 1, "role": "scout", "scout": "tests", "engine": "claude", "model": "opus", "session": "s-1", "detached": true, "attached": false }),
            json!({ "at": at, "kind": "session_end", "item": "R-12", "lane": 1, "role": "implementer", "engine": "claude", "model": "opus", "session": "s-1", "ms": 1140000.0, "cost": 1.25, "ok": false, "subtype": "error_max_turns", "error": "ran out", "limitHit": "5h" }),
            json!({ "at": at, "kind": "step_start", "item": "R-12", "step": "checks", "key": "slice 1 round 1", "again": true }),
            json!({ "at": at, "kind": "step_end", "item": "R-12", "step": "checks", "key": "k", "ms": 240000.0, "ok": false, "outcome": "failed", "checks": [{ "name": "test", "ok": false, "sec": 200 }], "lines": 40, "scouts": 2 }),
            json!({ "at": at, "kind": "claim", "answer": "no work" }),
            json!({ "at": at, "kind": "store_error", "item": "R-12", "op": "claim", "code": "E_LOCK", "error": "locked" }),
            json!({ "at": at, "kind": "error", "item": "R-12", "where": "checks", "error": "timeout" }),
            json!({ "at": at, "kind": "ci", "item": "R-12", "state": "passed", "ms": 60000.0 }),
            json!({ "at": at, "kind": "merged", "item": "R-12", "pr": 41, "chore": false }),
            json!({ "at": at, "kind": "host_call_undefined", "cmd": "play", "args": ["x"] }),
            json!({ "at": at, "kind": "restart_asked", "reason": "asked", "by": "james", "waitingOn": ["1 session"] }),
            json!({ "at": at, "kind": "restart_waiting", "reason": "asked", "waitingOn": ["1 session"], "ms": 5000.0 }),
            json!({ "at": at, "kind": "restart_exit", "reason": "asked", "by": "james", "drainMs": 900000.0 }),
            json!({ "at": at, "kind": "restart_back", "reason": "asked", "by": "james", "downMs": 120000.0 }),
            json!({ "at": at, "kind": "anomaly", "phase": "start", "id": "no_progress:R-12", "rule": "no_progress", "item": "R-12", "text": "stuck", "details": { "idleMs": 1 } }),
        ]
    }

    #[test]
    fn every_kind_the_agent_writes_reads_as_its_own_variant_and_writes_back_the_same_line() {
        let lines = real_lines();
        assert_eq!(lines.len(), 20, "one line shape per variant");
        let mut kinds = std::collections::HashSet::new();
        for line in &lines {
            let entry = Entry::from_value(line).expect("an event line");
            assert!(matches!(entry.body, Body::Known(_)), "typed: {line}");
            assert_eq!(entry.to_value(), *line, "round trip");
            kinds.insert(entry.kind());
        }
        assert_eq!(kinds.len(), 20);
    }

    #[test]
    fn an_unknown_kind_an_extra_field_and_a_missing_field_all_still_read() {
        let unknown = json!({ "at": "2026-10-15T08:00:00.000Z", "kind": "from_the_future", "item": "R-1", "n": 3 });
        let e = Entry::from_value(&unknown).unwrap();
        assert_eq!(e.kind(), "from_the_future");
        assert_eq!(e.item(), Some("R-1"));
        assert!(matches!(e.body, Body::Other { .. }));
        assert_eq!(e.to_value(), unknown, "kept whole");
        let extra =
            json!({ "at": "2026-10-15T08:00:00.000Z", "kind": "ci", "state": "passed", "runner": "woodpecker" });
        let e = Entry::from_value(&extra).unwrap();
        assert!(
            matches!(e.body, Body::Known(Event::Ci(_))),
            "unknown fields do not unseat a known kind"
        );
        assert_eq!(e.to_value(), extra, "and are written back");
        let bare = json!({ "at": "2026-10-15T08:00:00.000Z", "kind": "item_state" });
        assert!(matches!(
            Entry::from_value(&bare).unwrap().body,
            Body::Known(Event::ItemState(_))
        ));
        let mistyped = json!({ "at": "2026-10-15T08:00:00.000Z", "kind": "merged", "pr": "forty-one" });
        let e = Entry::from_value(&mistyped).unwrap();
        assert!(
            matches!(e.body, Body::Other { .. }),
            "a field of the wrong type falls back, it does not fail"
        );
        assert_eq!(e.to_value(), mistyped);
        assert!(Entry::from_value(&json!({ "at": "t", "kind": "" })).is_none());
        assert!(Entry::from_value(&json!({ "kind": "tick" })).is_none());
        assert!(Entry::from_value(&json!([1])).is_none());
    }

    #[test]
    fn an_unknown_kind_line_in_a_day_file_is_read_beside_the_known_ones() {
        let dir = tmp();
        let mut text = String::new();
        for line in [
            json!({ "at": "2026-10-15T08:00:00.000Z", "kind": "mystery", "x": 1 }),
            json!({ "at": "2026-10-15T08:01:00.000Z", "kind": "ci", "item": "R-1", "state": "passed" }),
        ] {
            text.push_str(&format!("{line}\n"));
        }
        std::fs::write(dir.path().join("2026-10-15.jsonl"), text).unwrap();
        let read = read_events(dir.path(), 0, i64::MAX);
        assert_eq!(read.iter().map(|e| e.kind()).collect::<Vec<_>>(), ["mystery", "ci"]);
    }

    #[test]
    fn an_event_added_in_a_project_writes_that_project_and_is_cleaned() {
        let mut log = EventLog::with_now(|| 0);
        let e = log.add_in(
            "alpha",
            Event::Error(crate::telemetry::event::Error {
                error: Some("gh said token ghp_abcdefghijklmnopqrstuvwxyz0123".into()),
                ..Default::default()
            }),
        );
        assert_eq!(e.project.as_deref(), Some("alpha"));
        let v = e.to_value();
        assert_eq!(v["project"], "alpha");
        assert_eq!(v["error"], "gh said token [redacted]");
    }

    #[test]
    fn iso_round_trips_the_fixture_instant() {
        let t0 = t0();
        assert_eq!(iso_utc_millis(t0), "2026-10-15T23:59:30.000Z");
        assert_eq!(parse_iso_ms("2026-10-15T23:59:30.000Z"), Some(t0));
    }
}
