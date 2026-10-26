//! The engine-neutral half of running a session, following `agent/src/engines/runner.rs` in
//! Retaskable.com: spawning the process, feeding it its prompt, teeing its output to the reader,
//! and waiting for it to exit — the same for every `Engine`. Picking a session's engine by id
//! (`engineOf`) lives here too.
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::claude::ClaudeEngine;
use super::engine::{Engine, EngineId, SessionResult, SessionSpec};

/// `engineOf(id)`: the `Engine` a session's `SessionSpec.engine` names; missing means Claude Code.
/// A Cursor engine is a later item — asking for it fails clearly instead of silently falling back.
pub fn engine_of(id: Option<EngineId>) -> Result<Box<dyn Engine>> {
    match id.unwrap_or(EngineId::Claude) {
        EngineId::Claude => Ok(Box::new(ClaudeEngine)),
        EngineId::Cursor => anyhow::bail!("the cursor engine isn't built yet"),
    }
}

/// Spawns `bin` with `args`, feeds `prompt` in on stdin, and reads the engine's output from stdout
/// line by line until the process exits — not knowing or caring which engine wrote any of it.
async fn spawn_and_read(
    bin: &Path,
    args: &[String],
    envs: &[(String, String)],
    cwd: &Path,
    prompt: &str,
    known_session_id: &str,
    mut reader: Box<dyn super::engine::EngineReader>,
) -> Result<SessionResult> {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;

    let mut stdin = child.stdin.take().context("session has no stdin")?;
    let prompt = prompt.to_string();
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
        if let Some(r) = reader.line(&line, known_session_id) {
            result = Some(r);
        }
    }

    let _ = write_prompt.await;
    let stderr_text = drain_stderr.await.unwrap_or_default();
    let status = child.wait().await.context("waiting for the session to exit")?;

    Ok(result.unwrap_or_else(|| reader.no_result(status.code(), &stderr_text, known_session_id)))
}

/// Runs one session end to end on `engine`: builds its invocation for `spec`, runs it, and reads
/// back its `SessionResult`. `bin` is `engine`'s already-resolved binary (`engine.binary()`,
/// resolved once per process, not re-resolved for every session). `known_session_id` is used when
/// a result doesn't carry its own `session_id` (and as the id on a crash with no result at all).
pub async fn run_session(
    engine: &dyn Engine,
    bin: &Path,
    spec: &SessionSpec,
    known_session_id: &str,
) -> Result<SessionResult> {
    spawn_and_read(
        bin,
        &engine.args(spec),
        &engine.env(spec),
        &spec.cwd,
        &engine.prompt(spec),
        known_session_id,
        engine.reader(spec),
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

    #[tokio::test]
    async fn spawn_and_read_returns_the_result_event_a_session_printed() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.jsonl");
        std::fs::write(
            &out,
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"total_cost_usd":1.25,"session_id":"sess-1"}"#,
        )
        .unwrap();
        let (bin, args) = print_file_command(&out);
        let result = spawn_and_read(
            &bin,
            &args,
            &[],
            dir.path(),
            "hi",
            "known",
            Box::new(SessionReader::new(false)),
        )
        .await
        .unwrap();
        assert!(result.ok);
        assert_eq!(result.subtype, "success");
        assert_eq!(result.structured, Some(json!({"status": "pr_opened"})));
        assert_eq!(result.cost_usd, 1.25);
        assert_eq!(result.session_id, "sess-1");
        assert!(!result.signed_out);
        assert!(!result.limit_hit);
    }

    #[tokio::test]
    async fn spawn_and_read_reports_no_result_when_the_session_exits_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("empty.jsonl");
        std::fs::write(&out, "").unwrap();
        let (bin, args) = print_file_command(&out);
        let result = spawn_and_read(
            &bin,
            &args,
            &[],
            dir.path(),
            "hi",
            "known",
            Box::new(SessionReader::new(false)),
        )
        .await
        .unwrap();
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
    fn engine_of_names_the_cursor_engine_as_not_built_yet() {
        let err = engine_of(Some(EngineId::Cursor)).map(|_| ()).unwrap_err();
        assert!(err.to_string().contains("cursor"));
    }

    #[tokio::test]
    async fn run_session_dispatches_through_the_engine_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.jsonl");
        std::fs::write(
            &out,
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened"},"session_id":"sess-1"}"#,
        )
        .unwrap();
        let (bin, args) = print_file_command(&out);
        // Stand in for the engine's own `args`/`env`/`prompt`: whatever it returns is exactly
        // what the runner spawns, proving the runner never special-cases an engine.
        struct StubEngine {
            args: Vec<String>,
        }
        impl Engine for StubEngine {
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
        }
        let engine = StubEngine { args };
        let spec = SessionSpec {
            engine: None,
            model: "sonnet".to_string(),
            prompt: "hi".to_string(),
            schema: json!({"type": "object"}),
            cwd: dir.path().to_path_buf(),
            max_turns: None,
            budget_usd: 0.0,
            start: SessionStart::Ephemeral,
        };
        let result = run_session(&engine, &bin, &spec, "known").await.unwrap();
        assert!(result.ok);
        assert_eq!(result.session_id, "sess-1");
    }
}
