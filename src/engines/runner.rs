//! The engine-neutral half of running a session, following `agent/src/engines/runner.rs` in
//! Retaskable.com: spawning the process, feeding it its prompt, teeing its output to the reader
//! and (when given a [`TranscriptSink`]) to the run's `.jsonl`, and waiting for it to exit — the
//! same for every `Engine`. Picking a session's engine by id (`engineOf`) lives here too. The
//! pool/orchestrator that would call this with a real project's paths, and resume support on top
//! of it, are still future slices (milestone 3 of docs/specs/agent/11-build-milestones.md); this
//! only makes the write side of the format `transcripts.rs` already reads real.
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::claude::ClaudeEngine;
use super::cursor::CursorEngine;
use super::engine::{Engine, EngineId, SessionResult, SessionSpec};
use super::pi::PiEngine;

/// `engineOf(id)`: the `Engine` a session's `SessionSpec.engine` names; missing means Claude Code.
pub fn engine_of(id: Option<EngineId>) -> Result<Box<dyn Engine>> {
    match id.unwrap_or(EngineId::Claude) {
        EngineId::Claude => Ok(Box::new(ClaudeEngine)),
        EngineId::Cursor => Ok(Box::new(CursorEngine)),
        EngineId::Pi => Ok(Box::new(PiEngine)),
    }
}

/// Where a session's raw output lines are teed as they arrive, on top of the reader
/// (docs/specs/engines/08-live-transcripts.md: "the raw lines go to the run's .jsonl ... after a
/// retaskable_meta line"). `path` is the run's own `<sessionId>.jsonl` — building it from a
/// project and item id (`paths::run_dir`) is the caller's job; the runner itself knows nothing of
/// projects or items.
pub struct TranscriptSink {
    pub path: PathBuf,
    /// The implementer's lane, when this session has one (`started_on`'s `lane`).
    pub lane: Option<u32>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The transcript's own header line, following `transcripts.rs`'s `started_on`/`read_transcripts`:
/// the engine and model this session started on, its lane when it has one, and when it started.
fn meta_line(spec: &SessionSpec, lane: Option<u32>) -> String {
    let mut meta = json!({
        "type": "retaskable_meta",
        "model": spec.model,
        "engine": spec.engine.unwrap_or(EngineId::Claude).to_string(),
        "at": now_ms(),
    });
    if let Some(lane) = lane {
        meta["lane"] = json!(lane);
    }
    meta.to_string()
}

/// Creates `sink.path` (and its run folder), writes the meta line, and returns it ready for
/// `spawn_and_read` to tee each raw line onto.
fn open_sink(sink: &TranscriptSink, spec: &SessionSpec) -> Result<std::fs::File> {
    if let Some(parent) = sink.path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut file = std::fs::File::create(&sink.path).with_context(|| format!("creating {}", sink.path.display()))?;
    use std::io::Write as _;
    writeln!(file, "{}", meta_line(spec, sink.lane)).context("writing the transcript's meta line")?;
    Ok(file)
}

/// What [`spawn_and_read`] needs to start the process and feed it its prompt, bundled so the
/// function takes one argument instead of six.
struct SpawnSpec<'a> {
    bin: &'a Path,
    args: &'a [String],
    envs: &'a [(String, String)],
    cwd: &'a Path,
    prompt: &'a str,
    known_session_id: &'a str,
}

/// Spawns `spec.bin` with `spec.args`, feeds `spec.prompt` in on stdin, and reads the engine's
/// output from stdout line by line until the process exits — not knowing or caring which engine
/// wrote any of it. Each raw line is teed to `sink` first, when there is one, so a crash partway
/// still leaves everything read so far on disk.
async fn spawn_and_read(
    spec: SpawnSpec<'_>,
    mut reader: Box<dyn super::engine::EngineReader>,
    mut sink: Option<std::fs::File>,
) -> Result<SessionResult> {
    let mut child = Command::new(spec.bin)
        .args(spec.args)
        .current_dir(spec.cwd)
        .envs(spec.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", spec.bin.display()))?;

    let mut stdin = child.stdin.take().context("session has no stdin")?;
    let prompt = spec.prompt.to_string();
    let write_prompt = tokio::spawn(async move {
        let _ = stdin.write_all(prompt.as_bytes()).await;
    });

    let mut stderr = child.stderr.take().context("session has no stderr")?;
    let drain_stderr = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf).await;
        buf
    });

    let stdout = child.stdout.take().context("session has no stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut result = None;
    while let Some(line) = lines.next_line().await.context("reading the session's output")? {
        if let Some(file) = sink.as_mut() {
            use std::io::Write as _;
            let _ = writeln!(file, "{line}");
        }
        if let Some(r) = reader.line(&line, spec.known_session_id) {
            result = Some(r);
        }
    }

    let _ = write_prompt.await;
    let stderr_text = drain_stderr.await.unwrap_or_default();
    let status = child.wait().await.context("waiting for the session to exit")?;

    Ok(result.unwrap_or_else(|| reader.no_result(status.code(), &stderr_text, spec.known_session_id)))
}

/// Runs one session end to end on `engine`: builds its invocation for `spec`, runs it, and reads
/// back its `SessionResult`. `bin` is `engine`'s already-resolved binary (`engine.binary()`,
/// resolved once per process, not re-resolved for every session). `known_session_id` is used when
/// a result doesn't carry its own `session_id` (and as the id on a crash with no result at all).
/// `sink` is `None` for a session with no transcript to keep (`SessionStart::Ephemeral`'s own
/// reason for existing); given one, its file is created (or replaced) before the session starts.
/// Calls `engine.prepare(spec)` first (a no-op for Claude Code; Cursor's own allowlist and isolate
/// config, sections 4 and 6), so a failed prepare never leaves an empty transcript file behind.
pub async fn run_session(
    engine: &dyn Engine,
    bin: &Path,
    spec: &SessionSpec,
    known_session_id: &str,
    sink: Option<&TranscriptSink>,
) -> Result<SessionResult> {
    engine.prepare(spec)?;
    let file = sink.map(|sink| open_sink(sink, spec)).transpose()?;
    spawn_and_read(
        SpawnSpec {
            bin,
            args: &engine.args(spec),
            envs: &engine.env(spec),
            cwd: &spec.cwd,
            prompt: &engine.prompt(spec),
            known_session_id,
        },
        engine.reader(spec),
        file,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::engines::SessionStart;
    use crate::engines::claude::SessionReader;

    #[cfg(windows)]
    fn print_file_command(file: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("cmd"),
            vec![
                "/C".to_string(),
                "type".to_string(),
                file.to_string_lossy().into_owned(),
            ],
        )
    }

    #[cfg(not(windows))]
    fn print_file_command(file: &Path) -> (PathBuf, Vec<String>) {
        (PathBuf::from("cat"), vec![file.to_string_lossy().into_owned()])
    }

    /// Runs `spawn_and_read` against a command that prints `out`'s contents, the shared shape
    /// both "a session printed this" tests below spawn.
    async fn read_printed_file(dir: &Path, out: &Path) -> SessionResult {
        let (bin, args) = print_file_command(out);
        spawn_and_read(
            SpawnSpec {
                bin: &bin,
                args: &args,
                envs: &[],
                cwd: dir,
                prompt: "hi",
                known_session_id: "known",
            },
            Box::new(SessionReader::new(false)),
            None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn spawn_and_read_returns_the_result_event_a_session_printed() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.jsonl");
        std::fs::write(
            &out,
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"total_cost_usd":1.25,"session_id":"sess-1"}"#,
        )
        .unwrap();
        let result = read_printed_file(dir.path(), &out).await;
        assert!(result.ok);
        assert_eq!(result.subtype, "success");
        assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
        assert_eq!(result.cost_usd, Some(1.25));
        assert_eq!(result.session_id, "sess-1");
        assert!(!result.signed_out);
        assert!(!result.limit_hit);
    }

    #[tokio::test]
    async fn spawn_and_read_reports_no_result_when_the_session_exits_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("empty.jsonl");
        std::fs::write(&out, "").unwrap();
        let result = read_printed_file(dir.path(), &out).await;
        assert!(!result.ok);
        assert_eq!(result.subtype, "exited");
        assert_eq!(result.session_id, "known");
        assert!(result.error.unwrap().contains("exited with code 0"));
    }

    #[test]
    fn engine_of_defaults_a_missing_id_to_claude_code() {
        assert_eq!(engine_of(None).unwrap().id(), EngineId::Claude);
        assert_eq!(engine_of(Some(EngineId::Claude)).unwrap().id(), EngineId::Claude);
    }

    #[test]
    fn engine_of_resolves_the_cursor_engine_by_id() {
        assert_eq!(engine_of(Some(EngineId::Cursor)).unwrap().id(), EngineId::Cursor);
    }

    #[test]
    fn engine_of_resolves_the_pi_engine_by_id() {
        assert_eq!(engine_of(Some(EngineId::Pi)).unwrap().id(), EngineId::Pi);
    }

    /// Stands in for an engine's own `args`/`env`/`prompt`/`reader`: whatever `args` it's given is
    /// exactly what the runner spawns, proving `run_session` never special-cases an engine.
    struct ArgsEngine {
        args: Vec<String>,
    }
    impl Engine for ArgsEngine {
        fn id(&self) -> EngineId {
            EngineId::Claude
        }
        fn binary(
            &self,
            _env: &dyn Fn(&str) -> Option<String>,
            _windows: bool,
            _path_dirs: &[PathBuf],
        ) -> Result<PathBuf> {
            unreachable!("not exercised by run_session")
        }
        fn args(&self, _spec: &SessionSpec) -> Vec<String> {
            self.args.clone()
        }
        fn env(&self, _spec: &SessionSpec) -> Vec<(String, String)> {
            vec![]
        }
        fn prompt(&self, spec: &SessionSpec) -> String {
            spec.prompt.clone()
        }
        fn reader(&self, spec: &SessionSpec) -> Box<dyn super::super::engine::EngineReader> {
            Box::new(SessionReader::new(spec.model == "local"))
        }
        fn summarize(&self, _event: &serde_json::Value, _at: i64) -> Vec<super::super::engine::SessionEvent> {
            unreachable!("not exercised by run_session")
        }
    }

    /// A command that prints one successful `result` event, and an `ArgsEngine` built to run it
    /// — the shared fixture both "`run_session` ran a successful session" tests below start from.
    fn success_session(dir: &Path) -> (PathBuf, ArgsEngine) {
        let out = dir.join("out.jsonl");
        std::fs::write(
            &out,
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"session_id":"sess-1"}"#,
        )
        .unwrap();
        let (bin, args) = print_file_command(&out);
        (bin, ArgsEngine { args })
    }

    #[tokio::test]
    async fn run_session_dispatches_through_the_engine_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, engine) = success_session(dir.path());
        let spec = SessionSpec {
            engine: None,
            model: "sonnet".to_string(),
            prompt: "hi".to_string(),
            schema: json!({"type": "object"}),
            cwd: dir.path().to_path_buf(),
            max_turns: None,
            budget_usd: 0.0,
            start: SessionStart::Ephemeral,
            readonly: false,
            cursor: None,
            local: None,
            sandbox: false,
        };
        let result = run_session(&engine, &bin, &spec, "known", None).await.unwrap();
        assert!(result.ok);
        assert_eq!(result.session_id, "sess-1");
    }

    // TranscriptSink: teeing a session's raw output to the run's own .jsonl

    #[tokio::test]
    async fn spawn_and_read_tees_the_sessions_raw_lines_to_the_sink_after_opening_it_itself() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.jsonl");
        let line =
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"sess-1"}"#;
        std::fs::write(&out, line).unwrap();
        let (bin, args) = print_file_command(&out);
        let sink_path = dir.path().join("deep").join("sess-1.jsonl");
        let file = open_sink(
            &TranscriptSink {
                path: sink_path.clone(),
                lane: Some(3),
            },
            &base_spec(dir.path()),
        )
        .unwrap();

        spawn_and_read(
            SpawnSpec {
                bin: &bin,
                args: &args,
                envs: &[],
                cwd: dir.path(),
                prompt: "hi",
                known_session_id: "known",
            },
            Box::new(SessionReader::new(false)),
            Some(file),
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(&sink_path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2);
        let meta: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(meta["type"], "retaskable_meta");
        assert_eq!(meta["model"], "sonnet");
        assert_eq!(meta["engine"], "claude");
        assert_eq!(meta["lane"], 3);
        assert!(meta["at"].is_i64());
        assert_eq!(lines[1], line);
    }

    #[tokio::test]
    async fn run_session_opens_the_sink_itself_so_a_transcript_reads_back_through_started_on_and_read_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, engine) = success_session(dir.path());
        let data_dir = tempfile::tempdir().unwrap();
        let sink = TranscriptSink {
            path: crate::paths::run_dir(data_dir.path(), "proj", "A1").join("sess-1.jsonl"),
            lane: Some(2),
        };

        let result = run_session(&engine, &bin, &base_spec(dir.path()), "sess-1", Some(&sink))
            .await
            .unwrap();
        assert!(result.ok);

        let started = crate::engines::transcripts::started_on(data_dir.path(), "proj", "A1", "sess-1").unwrap();
        assert_eq!(started.engine, EngineId::Claude);
        assert_eq!(started.model, "sonnet");
        assert_eq!(started.lane, Some(2));

        let transcripts = crate::engines::transcripts::read_transcripts(data_dir.path(), "proj", "A1");
        assert_eq!(transcripts.len(), 1);
        assert!(transcripts[0].events.iter().any(|e| e.text == "Finished: done"));
    }

    fn base_spec(cwd: &Path) -> SessionSpec {
        crate::engines::engine::test_spec(cwd, "hi")
    }
}
