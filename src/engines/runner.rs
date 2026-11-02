//! The engine-neutral half of running a session, following `agent/src/engines/runner.rs` in
//! Retaskable.com: spawning the process, feeding it its prompt, teeing its output to the reader
//! and (when given a [`TranscriptSink`]) to the run's `.jsonl`, and waiting for it to exit — the
//! same for every `Engine`. Picking a session's engine by id (`engineOf`) lives here too. The
//! pool/orchestrator that would call this with a real project's paths, and resume support on top
//! of it, are still future slices (milestone 3 of docs/specs/agent/11-build-milestones.md); this
//! only makes the write side of the format `transcripts.rs` already reads real.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::claude::ClaudeEngine;
use super::codex::CodexEngine;
use super::cursor::CursorEngine;
use super::engine::{Engine, EngineId, SessionResult, SessionSpec};
use super::pi::PiEngine;
use crate::process_tree::{KILL_LIMIT, ProcessTree};
use crate::telemetry::events::now_ms;

/// How long an `ends_on_result` engine's process has to exit on its own after its result.
const END_AFTER_RESULT: Duration = Duration::from_secs(5);

/// `engineOf(id)`: the `Engine` a session's `SessionSpec.engine` names; missing means Claude Code.
pub fn engine_of(id: Option<EngineId>) -> Result<Box<dyn Engine>> {
    match id.unwrap_or(EngineId::Claude) {
        EngineId::Claude => Ok(Box::new(ClaudeEngine)),
        EngineId::Cursor => Ok(Box::new(CursorEngine)),
        EngineId::Pi => Ok(Box::new(PiEngine)),
        EngineId::Codex => Ok(Box::new(CodexEngine)),
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
/// function takes one argument instead of seven.
struct SpawnSpec<'a> {
    bin: &'a Path,
    args: &'a [String],
    envs: &'a [(String, String)],
    cwd: &'a Path,
    prompt: &'a str,
    known_session_id: &'a str,
    /// Ends the process tree this long after a result, when the engine's process can linger.
    end_after_result: Option<Duration>,
    /// Kills the process tree once this long passes with no output line at all (the watchdog,
    /// 07-orchestrator-loop.md's `tuning.sessionQuietMinutes`), resetting on every line. `None`
    /// turns it off. Only watches while no result has been read yet: once one has, `end_after_result`
    /// (a much shorter wait) takes over instead.
    quiet: Option<Duration>,
    /// Given the session's process tree right after it joins, so a caller outside this loop — a
    /// restart or Quit (07.4-stopping-and-restarting.md), or this module's own test — can mark it
    /// `cut` and kill it while this loop is still reading its output. `None` when nothing needs to
    /// reach in, which is every real session today: nothing yet calls `ProcessTree::cut` on one.
    tree_tx: Option<tokio::sync::oneshot::Sender<Arc<ProcessTree>>>,
}

/// What a silent session's own `SessionResult` says, once the watchdog has killed it
/// (07-orchestrator-loop.md's "a session that prints nothing ... is killed"). Reusing the
/// existing `SessionResult::Failed` path (via `classify`), this is handed back to the project's
/// queue the same as any other crash, so the item gets picked up and retried.
fn killed_for_silence(known_session_id: &str, quiet: Duration) -> SessionResult {
    SessionResult {
        ok: false,
        subtype: "killed_for_silence".to_string(),
        structured: None,
        error: Some(format!("killed after {} minutes with no output", quiet.as_secs() / 60)),
        cost_usd: None,
        session_id: known_session_id.to_string(),
        limit_hit: false,
        signed_out: false,
        context_tokens: None,
        usage: None,
        tokens: None,
    }
}

/// What a cut session's own `SessionResult` says, in the same shape as [`killed_for_silence`]
/// (07.4-stopping-and-restarting.md's "A cut session ends as `interrupted`, not `failed`"):
/// something outside this session — a restart or Quit — called [`ProcessTree::cut`] and killed
/// its tree before it answered. `implementer::run_implementer` passes this on as a typed error
/// (`Interrupted`), the way a watchdog kill already is as `SilenceKilled`.
fn interrupted(known_session_id: &str) -> SessionResult {
    SessionResult {
        ok: false,
        subtype: "interrupted".to_string(),
        structured: None,
        error: Some("interrupted".to_string()),
        cost_usd: None,
        session_id: known_session_id.to_string(),
        limit_hit: false,
        signed_out: false,
        context_tokens: None,
        usage: None,
        tokens: None,
    }
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
    let mut command = Command::new(spec.bin);
    command
        .args(spec.args)
        .current_dir(spec.cwd)
        .envs(spec.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ProcessTree::prepare(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", spec.bin.display()))?;
    let tree = ProcessTree::join(&child).context("putting the session in its own process tree")?;
    if let Some(tx) = spec.tree_tx {
        let _ = tx.send(tree.clone());
    }

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
    let mut ends_at = None;
    let mut silenced = false;
    loop {
        let next = match ends_at {
            Some(at) => match tokio::time::timeout_at(at, lines.next_line()).await {
                Ok(next) => next,
                Err(_) => {
                    let tree = tree.clone();
                    let _ = tokio::task::spawn_blocking(move || tree.kill_and_wait(KILL_LIMIT)).await;
                    break;
                }
            },
            None => match spec.quiet {
                Some(quiet) => match tokio::time::timeout(quiet, lines.next_line()).await {
                    Ok(next) => next,
                    Err(_) => {
                        let tree = tree.clone();
                        let _ = tokio::task::spawn_blocking(move || tree.kill_and_wait(KILL_LIMIT)).await;
                        silenced = true;
                        break;
                    }
                },
                None => lines.next_line().await,
            },
        };
        let Some(line) = next.context("reading the session's output")? else {
            break;
        };
        if let Some(file) = sink.as_mut() {
            use std::io::Write as _;
            let _ = writeln!(file, "{line}");
        }
        if let Some(r) = reader.line(&line, spec.known_session_id) {
            result = Some(r);
            if let Some(wait) = spec.end_after_result {
                ends_at.get_or_insert_with(|| tokio::time::Instant::now() + wait);
            }
        }
    }

    let _ = write_prompt.await;
    let stderr_text = drain_stderr.await.unwrap_or_default();
    let status = child.wait().await.context("waiting for the session to exit")?;

    Ok(if tree.was_cut() {
        interrupted(spec.known_session_id)
    } else if silenced {
        killed_for_silence(spec.known_session_id, spec.quiet.expect("set when silenced"))
    } else {
        result.unwrap_or_else(|| reader.no_result(status.code(), &stderr_text, spec.known_session_id))
    })
}

/// Runs one session end to end on `engine`: builds its invocation for `spec`, runs it, and reads
/// back its `SessionResult`. `bin` is `engine`'s already-resolved binary (`engine.binary()`,
/// resolved once per process, not re-resolved for every session). `known_session_id` is used when
/// a result doesn't carry its own `session_id` (and as the id on a crash with no result at all).
/// `sink` is `None` for a session with no transcript to keep (`SessionStart::Ephemeral`'s own
/// reason for existing); given one, its file is created (or replaced) before the session starts.
/// Calls `engine.prepare(spec)` first (a no-op for Claude Code; Cursor's own allowlist and isolate
/// config, sections 4 and 6), so a failed prepare never leaves an empty transcript file behind.
/// `quiet` is the project's watchdog timeout (`tuning.sessionQuietMinutes`); `None` leaves it off.
pub async fn run_session(
    engine: &dyn Engine,
    bin: &Path,
    spec: &SessionSpec,
    known_session_id: &str,
    sink: Option<&TranscriptSink>,
    quiet: Option<Duration>,
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
            end_after_result: engine.ends_on_result().then_some(END_AFTER_RESULT),
            quiet,
            tree_tx: None,
        },
        engine.reader(spec),
        file,
    )
    .await
}

#[cfg(test)]
mod tests {
    use crate::env::Env;
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

    /// The `SpawnSpec` every test below shares (no envs, prompt "hi", known session id "known"),
    /// varying only the command, its two watchdog timeouts, and (when a test needs one) its
    /// `tree_tx`.
    fn spec<'a>(
        bin: &'a Path,
        args: &'a [String],
        cwd: &'a Path,
        end_after_result: Option<Duration>,
        quiet: Option<Duration>,
        tree_tx: Option<tokio::sync::oneshot::Sender<Arc<ProcessTree>>>,
    ) -> SpawnSpec<'a> {
        SpawnSpec {
            bin,
            args,
            envs: &[],
            cwd,
            prompt: "hi",
            known_session_id: "known",
            end_after_result,
            quiet,
            tree_tx,
        }
    }

    /// Runs `spawn_and_read` with [`spec`]'s shared shape and no transcript sink, varying only the
    /// command, its reader, and the two watchdog timeouts under test.
    async fn run_spawn(
        bin: &Path,
        args: &[String],
        cwd: &Path,
        end_after_result: Option<Duration>,
        quiet: Option<Duration>,
        reader: Box<dyn super::super::engine::EngineReader>,
    ) -> SessionResult {
        spawn_and_read(spec(bin, args, cwd, end_after_result, quiet, None), reader, None)
            .await
            .unwrap()
    }

    /// Runs `spawn_and_read` against a command that prints `out`'s contents, the shared shape
    /// both "a session printed this" tests below spawn.
    async fn read_printed_file(dir: &Path, out: &Path) -> SessionResult {
        let (bin, args) = print_file_command(out);
        run_spawn(&bin, &args, dir, None, None, Box::new(SessionReader::new(false))).await
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

    /// A command that waits `secs` seconds and prints nothing at all, like a session gone quiet.
    #[cfg(windows)]
    fn silent_command(secs: u32) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("cmd"),
            vec!["/C".to_string(), format!("ping -n {} 127.0.0.1 >nul", secs + 1)],
        )
    }

    #[cfg(not(windows))]
    fn silent_command(secs: u32) -> (PathBuf, Vec<String>) {
        (PathBuf::from("sleep"), vec![secs.to_string()])
    }

    #[tokio::test]
    async fn spawn_and_read_kills_a_session_thats_been_silent_past_the_quiet_timeout() {
        // The silent command runs far longer than the quiet timeout so a slow `taskkill`/`kill`
        // under a loaded CI box (RUST_TEST_THREADS is capped, but still shares the host with
        // other tests' own subprocesses) still finishes well before the command would have
        // exited on its own, instead of the margin itself flaking the assertion.
        let dir = tempfile::tempdir().unwrap();
        let (bin, args) = silent_command(20);
        let started = std::time::Instant::now();
        let result = run_spawn(
            &bin,
            &args,
            dir.path(),
            None,
            Some(Duration::from_millis(300)),
            Box::new(SessionReader::new(false)),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it waited out the silent process instead of killing it"
        );
        assert!(!result.ok);
        assert_eq!(result.subtype, "killed_for_silence");
        assert_eq!(result.session_id, "known");
        assert!(result.error.unwrap().contains("no output"));
    }

    #[tokio::test]
    async fn spawn_and_read_kills_a_silent_sessions_grandchild_too() {
        // A silent session that itself started a grandchild (e.g. a build or a dev server): the
        // watchdog's kill has to take the whole tree with it, not just the direct child it was
        // handed, which `crate::lanes::kill_tree(pid)` alone (walking only that pid's own
        // descendants at kill time) could still miss if the grandchild had reparented.
        let dir = tempfile::tempdir().unwrap();
        let (bin, args, ticks) = crate::test_support::grandchild_tick_command(dir.path());
        let result = run_spawn(
            &bin,
            &args,
            dir.path(),
            None,
            Some(Duration::from_millis(600)),
            Box::new(SessionReader::new(false)),
        )
        .await;
        assert!(!result.ok);
        assert_eq!(result.subtype, "killed_for_silence");

        let size_right_after = std::fs::metadata(&ticks).unwrap().len();
        assert!(
            size_right_after > 0,
            "the grandchild never ticked before the watchdog's kill"
        );
        std::thread::sleep(Duration::from_millis(400));
        let size_later = std::fs::metadata(&ticks).unwrap().len();
        assert_eq!(
            size_right_after, size_later,
            "the grandchild kept touching its file after the watchdog's kill"
        );
    }

    #[tokio::test]
    async fn spawn_and_read_reports_interrupted_when_something_outside_cuts_the_tree_mid_session() {
        // Something outside this session's own loop (a restart or Quit, 07.4-stopping-and-
        // restarting.md) cuts its tree and kills it while it's still running, before it ever
        // printed a result: that must come back as `interrupted`, not a watchdog silence-kill or
        // a plain no-result crash.
        let dir = tempfile::tempdir().unwrap();
        let (bin, args) = silent_command(20);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let run = spawn_and_read(
            spec(&bin, &args, dir.path(), None, None, Some(tx)),
            Box::new(SessionReader::new(false)),
            None,
        );
        let cut = async move {
            let tree = rx.await.unwrap();
            tree.cut();
            let _ = tokio::task::spawn_blocking(move || tree.kill_and_wait(KILL_LIMIT)).await;
        };
        let (result, ()) = tokio::join!(run, cut);
        let result = result.unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "interrupted");
        assert_eq!(result.session_id, "known");
    }

    /// A command that prints a line, waits `secs` seconds, then prints `out`'s contents —
    /// proving the quiet timer resets on any output, not just a result.
    #[cfg(windows)]
    fn print_wait_then_print_file_command(secs: u32, file: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("cmd"),
            vec![
                "/C".to_string(),
                format!(
                    "echo still here & ping -n {} 127.0.0.1 >nul & type {}",
                    secs + 1,
                    file.display()
                ),
            ],
        )
    }

    #[cfg(not(windows))]
    fn print_wait_then_print_file_command(secs: u32, file: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("sh"),
            vec![
                "-c".to_string(),
                format!("echo still here; sleep {secs}; cat '{}'", file.display()),
            ],
        )
    }

    #[tokio::test]
    async fn spawn_and_read_resets_the_quiet_timer_on_every_line_not_just_a_result() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.jsonl");
        std::fs::write(
            &out,
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{},"session_id":"sess-1"}"#,
        )
        .unwrap();
        // Each individual gap (the startup print, then before the file's contents) is well under
        // the quiet timeout, even though their sum isn't — proving the timer resets per line.
        let (bin, args) = print_wait_then_print_file_command(1, &out);
        let result = run_spawn(
            &bin,
            &args,
            dir.path(),
            None,
            Some(Duration::from_secs(3)),
            Box::new(SessionReader::new(false)),
        )
        .await;
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.session_id, "sess-1");
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

    #[test]
    fn engine_of_resolves_the_codex_engine_by_id() {
        assert_eq!(engine_of(Some(EngineId::Codex)).unwrap().id(), EngineId::Codex);
    }

    /// A command that prints `out`'s contents and then lingers for 30 seconds, like a Codex whose
    /// sandboxed child is still running after the turn is done.
    #[cfg(windows)]
    fn print_then_linger_command(file: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("cmd"),
            vec![
                "/C".to_string(),
                format!("type {} & ping -n 30 127.0.0.1 >nul", file.display()),
            ],
        )
    }

    #[cfg(not(windows))]
    fn print_then_linger_command(file: &Path) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("sh"),
            vec!["-c".to_string(), format!("cat '{}'; sleep 30", file.display())],
        )
    }

    #[tokio::test]
    async fn a_process_that_lingers_after_its_result_is_ended_and_the_result_stands() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("codex.jsonl");
        std::fs::write(
            &out,
            [
                json!({"type": "thread.started", "thread_id": "t-linger"}),
                json!({"type": "item.completed", "item": {"type": "agent_message", "text": "```json
{\"ok\":true}
```"}}),
                json!({"type": "turn.completed", "usage": {"input_tokens": 1, "output_tokens": 2}}),
            ]
            .iter()
            .map(|line| {
                format!(
                    "{line}
"
                )
            })
            .collect::<String>(),
        )
        .unwrap();
        let (bin, args) = print_then_linger_command(&out);
        let started = std::time::Instant::now();
        let result = run_spawn(
            &bin,
            &args,
            dir.path(),
            Some(Duration::from_millis(300)),
            None,
            Box::new(crate::engines::codex::CodexReader::new(json!({"type": "object"}))),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "it didn't wait out the lingering process"
        );
        assert!(result.ok, "{:?}", result.error);
        assert_eq!(result.structured, Some(json!({"ok": true})));
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
        fn binary(&self, _env: &Env, _windows: bool, _path_dirs: &[PathBuf]) -> Result<PathBuf> {
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
            shell: true,
            cursor: None,
            local: None,
            sandbox: false,
        };
        let result = run_session(&engine, &bin, &spec, "known", None, None).await.unwrap();
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
            spec(&bin, &args, dir.path(), None, None, None),
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

        let result = run_session(&engine, &bin, &base_spec(dir.path()), "sess-1", Some(&sink), None)
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
