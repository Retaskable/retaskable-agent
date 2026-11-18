//! `session_history` (docs/specs/expert-chat.md; Retaskable.com's `agent/src/expert/history.ts`): an expert session's
//! past turns, read from its run's `.jsonl` on this computer and summarized as its live steps are. The hub passes the
//! answer on and keeps none of it. Pure: the files are read here, nothing else is touched.
use std::path::Path;

use serde_json::{Value, json};

use super::claude_sessions::person_text;
use crate::engines::engine::{EventKind, SessionEvent};
use crate::engines::summarize::summarize;
use crate::text::clip;

/// The most steps one answer carries: whole turns, newest first, the oldest one cut to fit.
pub const HISTORY_LIMIT: usize = 200;

/// The longest message of the person's an earlier Claude Code turn carries, as `expert_say`'s limit.
const SAID_MAX: usize = 20_000;

/// One run of the session: when it started (its `retaskable_meta` line) and its steps. An earlier Claude Code turn
/// carries the person's message that started it too.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub at: i64,
    pub events: Vec<SessionEvent>,
    pub said: Option<String>,
}

impl Turn {
    fn event_json(e: &SessionEvent) -> Value {
        let kind = match e.kind {
            EventKind::Tool => "tool",
            EventKind::Text => "text",
            EventKind::Result => "result",
        };
        json!({ "at": e.at, "kind": kind, "text": e.text })
    }

    pub fn to_json(&self) -> Value {
        let mut t = json!({ "at": self.at, "events": self.events.iter().map(Self::event_json).collect::<Vec<_>>() });
        if let Some(said) = &self.said {
            t["said"] = json!(said);
        }
        t
    }
}

fn json_lines(file: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The run's turns, oldest first: each meta line starts one. Lines before the first meta line make a turn at 0.
pub fn read_turns(file: &Path) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for ev in json_lines(file) {
        if ev.get("type").and_then(Value::as_str) == Some("retaskable_meta") {
            let at = ev.get("at").and_then(Value::as_i64).unwrap_or(0);
            turns.push(Turn {
                at,
                events: Vec::new(),
                said: None,
            });
        } else {
            if turns.is_empty() {
                turns.push(Turn {
                    at: 0,
                    events: Vec::new(),
                    said: None,
                });
            }
            let turn = turns.last_mut().expect("a turn was just pushed");
            let at = turn.at;
            turn.events.extend(summarize(&ev, at));
        }
    }
    turns
}

/// A background task's notice, as Claude Code writes it into the conversation (`noticeText`): "" when it isn't one.
fn notice_text(ev: &Value) -> String {
    let content = |v: &Value| match v.pointer("/message/content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => (blocks.iter())
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    if ev.get("type").and_then(Value::as_str) != Some("user")
        || ev.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return String::new();
    }
    let text = content(ev);
    let is_notice = ev.pointer("/origin/kind").and_then(Value::as_str) == Some("task-notification")
        || text.trim_start().starts_with("<task-notification>");
    if !is_notice {
        return String::new();
    }
    let tag = |name: &str| -> String {
        let (open, close) = (format!("<{name}>"), format!("</{name}>"));
        text.find(&open)
            .and_then(|i| {
                let rest = &text[i + open.len()..];
                rest.find(&close).map(|j| rest[..j].trim().to_string())
            })
            .unwrap_or_default()
    };
    let head = Some(tag("summary")).filter(|s| !s.is_empty());
    let tail = Some(tag("event"))
        .filter(|s| !s.is_empty())
        .or_else(|| Some(tag("result")));
    [
        head.or(Some("Background task finished".to_string())),
        tail.filter(|s| !s.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn parse_time(ev: &Value) -> Option<i64> {
    crate::telemetry::events::parse_iso_ms(ev.get("timestamp")?.as_str()?)
}

/// A continued session's Claude Code conversation as turns: each message of the person's starts one, and Claude's
/// replies and steps after it are its events, summarized as a run's are. A background task's notice is a step of the
/// turn it lands in. Subagents' lines are left out.
pub fn claude_turns(file: &Path) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for ev in json_lines(file) {
        let at = parse_time(&ev).or_else(|| turns.last().map(|t| t.at)).unwrap_or(0);
        let said = person_text(&ev);
        let notice = if turns.is_empty() {
            String::new()
        } else {
            notice_text(&ev)
        };
        if !said.is_empty() {
            turns.push(Turn {
                at,
                events: Vec::new(),
                said: Some(clip(&said, SAID_MAX)),
            });
        } else if !notice.is_empty() {
            turns
                .last_mut()
                .expect("a notice follows a turn")
                .events
                .push(SessionEvent {
                    at,
                    kind: EventKind::Text,
                    text: clip(&notice, 800),
                });
        } else if ev.get("type").and_then(Value::as_str) == Some("assistant")
            && ev.get("isSidechain").and_then(Value::as_bool) != Some(true)
            && let Some(turn) = turns.last_mut()
        {
            turn.events.extend(summarize(&ev, at));
        }
    }
    turns
}

/// What a request for history asks (`before`, `limit`) and whether the session's last turn is still running.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pick {
    pub before: Option<i64>,
    pub limit: Option<usize>,
    pub running: bool,
}

/// The turns that started before `before` (all of them when it's absent), newest kept first, up to `limit` steps; a
/// running turn is left out, as `watch` shows it live. `more` when older turns are left.
pub fn pick_turns(turns: &[Turn], pick: Pick) -> (Vec<Turn>, bool) {
    let limit = pick.limit.unwrap_or(HISTORY_LIMIT).clamp(1, HISTORY_LIMIT);
    let upto = if pick.running {
        turns.len().saturating_sub(1)
    } else {
        turns.len()
    };
    let done: Vec<&Turn> = turns[..upto]
        .iter()
        .filter(|t| (!t.events.is_empty() || t.said.is_some()) && pick.before.is_none_or(|b| t.at < b))
        .collect();
    let mut out: Vec<Turn> = Vec::new();
    let mut left = limit;
    let mut i = done.len();
    while i > 0 && left > 0 {
        i -= 1;
        let t = done[i];
        let mut kept = t.clone();
        if t.events.len() > left {
            kept.events = t.events[t.events.len() - left..].to_vec();
        }
        left = left.saturating_sub(t.events.len().max(1));
        out.insert(0, kept);
    }
    (out, i > 0)
}

/// The answer, `{ turns, more }`.
pub fn answer(turns: &[Turn], pick: Pick) -> Value {
    let (turns, more) = pick_turns(turns, pick);
    json!({ "turns": turns.iter().map(Turn::to_json).collect::<Vec<_>>(), "more": more })
}

#[cfg(test)]
mod tests;
