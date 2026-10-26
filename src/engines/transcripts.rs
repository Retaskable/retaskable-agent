//! Reads a run's transcripts back from disk, each with the engine that wrote it, following
//! `agent/src/engines/transcripts.ts` in Retaskable.com and docs/specs/engines/02-the-engine-layer.md.
//! Nothing in this crate writes a run's `.jsonl` files yet — that lands with the orchestrator/pool
//! that owns a session's lifecycle — but the format is already fixed, so reading it back doesn't
//! need to wait.
use std::io::Read as _;
use std::path::Path;

use serde_json::Value;

use super::claude::ClaudeEngine;
use super::engine::{Engine, EngineId, EventKind, SessionEvent, engine_id_from_str};
use super::runner::engine_of;
use crate::paths::run_dir;

/// The engine, model and lane a session started on, from the meta line at the top of its
/// transcript (`startedOn`).
#[derive(Debug, Clone, PartialEq)]
pub struct StartedOn {
    pub engine: EngineId,
    pub model: String,
    pub lane: Option<u32>,
}

/// One transcript, read back for the drawer (`readTranscripts`'s own per-file shape, minus the
/// internal `started` sort key it never returns).
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub id: String,
    pub role: String,
    pub model: String,
    pub events: Vec<SessionEvent>,
}

/// An `Engine` for `engine_id`, falling back to Claude Code when that engine isn't built yet
/// (e.g. a transcript recorded `"cursor"` before that engine existed in this port) — reading a
/// transcript back should degrade, never panic, over a session it can't fully re-describe.
fn engine_for(engine_id: EngineId) -> Box<dyn Engine> {
    engine_of(Some(engine_id)).unwrap_or_else(|_| Box::new(ClaudeEngine))
}

/// `startedOn(project, itemId, sessionId)`: only the transcript's first 1024 bytes are read, since
/// that's always enough to hold the one meta line at its top.
pub fn started_on(data_dir: &Path, project: &str, item_id: &str, session_id: &str) -> Option<StartedOn> {
    let file = run_dir(data_dir, project, item_id).join(format!("{session_id}.jsonl"));
    let mut handle = std::fs::File::open(file).ok()?;
    let mut buf = [0u8; 1024];
    let n = handle.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let first_line = text.split('\n').next()?;
    let meta: Value = serde_json::from_str(first_line).ok()?;
    let engine = engine_id_from_str(meta.get("engine").and_then(Value::as_str));
    let model = meta.get("model").and_then(Value::as_str)?.to_string();
    let lane = meta.get("lane").and_then(Value::as_u64).map(|v| v as u32);
    Some(StartedOn { engine, model, lane })
}

/// `readTranscripts(project, itemId)`: every `.jsonl` file in the run's folder, oldest first by
/// its meta line's `at` (or the file's own modified time when it never had one).
pub fn read_transcripts(data_dir: &Path, project: &str, item_id: &str) -> Vec<Transcript> {
    let dir = run_dir(data_dir, project, item_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut found: Vec<(i64, Transcript)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        let mut role = "session".to_string();
        let mut model = String::new();
        let mut engine_id = EngineId::Claude;
        let mut started = file_modified_ms(&path);
        let mut events = Vec::new();

        for line in text.split('\n').filter(|l| !l.is_empty()) {
            let Ok(ev) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if ev.get("type").and_then(Value::as_str) == Some("retaskable_meta") {
                let scout = ev.get("scout").and_then(Value::as_str);
                let event_role = ev.get("role").and_then(Value::as_str).unwrap_or("");
                role = match scout {
                    Some(scout) => format!("{event_role}: {scout}"),
                    None => event_role.to_string(),
                };
                model = ev.get("model").and_then(Value::as_str).unwrap_or("").to_string();
                engine_id = engine_id_from_str(ev.get("engine").and_then(Value::as_str));
                let at = ev.get("at").and_then(Value::as_i64).unwrap_or(started);
                started = started.min(at);
                events.push(SessionEvent {
                    at,
                    kind: EventKind::Result,
                    text: format!("— {role} on {model} —"),
                });
                continue;
            }
            events.extend(engine_for(engine_id).summarize(&ev, started));
        }

        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        found.push((
            started,
            Transcript {
                id,
                role,
                model,
                events,
            },
        ));
    }

    found.sort_by_key(|(started, _)| *started);
    found.into_iter().map(|(_, transcript)| transcript).collect()
}

/// A file's modified time in epoch milliseconds, the fallback sort key for a transcript whose
/// meta line never said when it started.
fn file_modified_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Writes a run's transcript files under a fresh temp data directory; objects become JSON
    /// lines, strings are written as they are (mirroring `transcripts.test.ts`'s `writeRun`).
    fn write_run(data_dir: &Path, project: &str, item_id: &str, files: &[(&str, Vec<Value>)]) {
        let dir = run_dir(data_dir, project, item_id);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, lines) in files {
            let text = lines
                .iter()
                .map(|l| match l {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(dir.join(name), text).unwrap();
        }
    }

    #[test]
    fn an_item_with_no_run_folder_has_no_transcripts() {
        let data_dir = tempfile::tempdir().unwrap();
        assert_eq!(read_transcripts(data_dir.path(), "proj", "A1"), Vec::new());
    }

    #[test]
    fn transcripts_come_back_oldest_first_named_by_role_and_scout_with_the_meta_line_as_a_header() {
        let data_dir = tempfile::tempdir().unwrap();
        write_run(
            data_dir.path(),
            "proj",
            "A1",
            &[
                (
                    "late.jsonl",
                    vec![
                        json!({"type": "retaskable_meta", "role": "implementer", "model": "opus", "engine": "claude", "lane": 1, "at": 2000}),
                        json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": "npm test"}}]}}),
                        json!("not json, so it is skipped"),
                        json!({"type": "result", "subtype": "success"}),
                    ],
                ),
                (
                    "early.jsonl",
                    vec![
                        json!({"type": "retaskable_meta", "role": "reviewer", "scout": "security", "model": "sonnet", "at": 1000}),
                    ],
                ),
                ("notes.txt", vec![json!("not a transcript")]),
            ],
        );

        assert_eq!(
            read_transcripts(data_dir.path(), "proj", "A1"),
            vec![
                Transcript {
                    id: "early".to_string(),
                    role: "reviewer: security".to_string(),
                    model: "sonnet".to_string(),
                    events: vec![SessionEvent {
                        at: 1000,
                        kind: EventKind::Result,
                        text: "— reviewer: security on sonnet —".to_string(),
                    }],
                },
                Transcript {
                    id: "late".to_string(),
                    role: "implementer".to_string(),
                    model: "opus".to_string(),
                    events: vec![
                        SessionEvent {
                            at: 2000,
                            kind: EventKind::Result,
                            text: "— implementer on opus —".to_string()
                        },
                        SessionEvent {
                            at: 2000,
                            kind: EventKind::Tool,
                            text: "Bash: npm test".to_string()
                        },
                        SessionEvent {
                            at: 2000,
                            kind: EventKind::Result,
                            text: "Finished: done".to_string()
                        },
                    ],
                },
            ]
        );
    }

    #[test]
    fn started_on_reads_the_engine_model_and_lane_from_a_transcripts_first_line() {
        let data_dir = tempfile::tempdir().unwrap();
        write_run(
            data_dir.path(),
            "proj",
            "A1",
            &[
                (
                    "chat-1.jsonl",
                    vec![
                        json!({"type": "retaskable_meta", "role": "implementer", "model": "composer", "engine": "cursor", "lane": 2, "at": 1}),
                    ],
                ),
                (
                    "s-2.jsonl",
                    vec![json!({"type": "retaskable_meta", "role": "reviewer", "model": "sonnet", "at": 1})],
                ),
                ("broken.jsonl", vec![json!("not json")]),
            ],
        );

        assert_eq!(
            started_on(data_dir.path(), "proj", "A1", "chat-1"),
            Some(StartedOn {
                engine: EngineId::Cursor,
                model: "composer".to_string(),
                lane: Some(2)
            })
        );
        assert_eq!(
            started_on(data_dir.path(), "proj", "A1", "s-2"),
            Some(StartedOn {
                engine: EngineId::Claude,
                model: "sonnet".to_string(),
                lane: None
            })
        );
        assert_eq!(
            started_on(data_dir.path(), "proj", "A1", "broken"),
            None,
            "a first line that isn't JSON"
        );
        assert_eq!(
            started_on(data_dir.path(), "proj", "A1", "missing"),
            None,
            "no such transcript"
        );
    }
}
