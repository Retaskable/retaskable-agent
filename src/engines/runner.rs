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
    // A Claude session on the local model records `local`, whatever Claude Code model name it was started with.
    let local = spec.local.is_some() && spec.engine.unwrap_or(EngineId::Claude) == EngineId::Claude;
    let mut meta = json!({
        "type": "retaskable_meta",
        "model": if local { "local" } else { spec.model.as_str() },
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

/// The two clocks that kill a session still running: `quiet` is the watchdog (no output line for that
/// long), `longest` the Cursor runaway guard (that long since spawn, even while lines keep arriving).
/// `None` turns each off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Watch {
    pub quiet: Option<Duration>,
    pub longest: Option<Duration>,
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
    watch: Watch,
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

/// What a Cursor session's own `SessionResult` says once it has run past `budget.cursorMaxMinutes`.
fn killed_for_time(known_session_id: &str, longest: Duration) -> SessionResult {
    SessionResult {
        subtype: "cursor_time".to_string(),
        error: Some(format!(
            "killed after {} minutes, past its time limit",
            longest.as_secs() / 60
        )),
        ..killed_for_silence(known_session_id, longest)
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
/// The session's process wouldn't start (the OS refused to spawn it, for example while Claude Code is
/// updating), so nothing ran: the caller can start it again (07-orchestrator-loop.md).
#[derive(Debug)]
pub struct CouldNotStart(pub String);

impl std::fmt::Display for CouldNotStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CouldNotStart {}

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
        .map_err(|e| anyhow::Error::new(CouldNotStart(format!("starting {}: {e}", spec.bin.display()))))?;
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
    let mut overtime = false;
    let deadline = spec.watch.longest.map(|l| tokio::time::Instant::now() + l);
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
            None => {
                let quiet_at = spec.watch.quiet.map(|q| tokio::time::Instant::now() + q);
                let (at, by_time) = match (quiet_at, deadline) {
                    (Some(q), Some(d)) if d <= q => (Some(d), true),
                    (Some(q), _) => (Some(q), false),
                    (None, d) => (d, true),
                };
                match at {
                    Some(at) => match tokio::time::timeout_at(at, lines.next_line()).await {
                        Ok(next) => next,
                        Err(_) => {
                            let tree = tree.clone();
                            let _ = tokio::task::spawn_blocking(move || tree.kill_and_wait(KILL_LIMIT)).await;
                            overtime = by_time;
                            silenced = !by_time;
                            break;
                        }
                    },
                    None => lines.next_line().await,
                }
            }
        };
        let Some(line) = next.context("reading the session's output")? else {
            break;
        };
        crate::orchestrator::live::sessions::touch();
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
    } else if overtime {
        killed_for_time(spec.known_session_id, spec.watch.longest.expect("set when overtime"))
    } else if silenced {
        killed_for_silence(spec.known_session_id, spec.watch.quiet.expect("set when silenced"))
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
/// `watch` is the project's watchdog timeout (`tuning.sessionQuietMinutes`) and, for Cursor, its time limit.
pub async fn run_session(
    engine: &dyn Engine,
    bin: &Path,
    spec: &SessionSpec,
    known_session_id: &str,
    sink: Option<&TranscriptSink>,
    watch: Watch,
) -> Result<SessionResult> {
    run_session_tracked(engine, bin, spec, known_session_id, sink, watch, None).await
}

/// `spec` with no dollar cap, when `on_subscription` and it has one.
fn uncapped_on(spec: &SessionSpec, on_subscription: bool) -> Option<SessionSpec> {
    (on_subscription && spec.budget_usd > 0.0).then(|| SessionSpec {
        budget_usd: 0.0,
        ..spec.clone()
    })
}

/// [`run_session`], handing the session's process tree to `tree_tx` once it has started, so a run can cut it.
pub async fn run_session_tracked(
    engine: &dyn Engine,
    bin: &Path,
    spec: &SessionSpec,
    known_session_id: &str,
    sink: Option<&TranscriptSink>,
    watch: Watch,
    tree_tx: Option<tokio::sync::oneshot::Sender<Arc<ProcessTree>>>,
) -> Result<SessionResult> {
    // A Claude session is held to its model's slot until it ends; Cursor, pi and Codex count toward `concurrency` only.
    let slots = (cfg!(test) || crate::tiers::model_caps_on())
        .then(super::model_slots::global)
        .flatten();
    let _slot = match slots {
        Some(slots) if spec.engine.unwrap_or_default() == EngineId::Claude => Some(slots.acquire(&spec.model).await),
        _ => None,
    };
    // On a Claude subscription the usage window is the limit, so no session carries a dollar cap.
    let uncapped = uncapped_on(spec, crate::tiers::machine_on_subscription(now_ms() / 1000));
    let spec = uncapped.as_ref().unwrap_or(spec);
    engine.prepare(spec)?;
    let file = sink.map(|sink| open_sink(sink, spec)).transpose()?;
    let result = spawn_and_read(
        SpawnSpec {
            bin,
            args: &engine.args(spec),
            envs: &engine.env(spec),
            cwd: &spec.cwd,
            prompt: &engine.prompt(spec),
            known_session_id,
            end_after_result: engine.ends_on_result().then_some(END_AFTER_RESULT),
            watch,
            tree_tx,
        },
        engine.reader(spec),
        file,
    )
    .await?;
    crate::orchestrator::live::sessions::record_session_end(spec.engine.unwrap_or_default(), &result);
    Ok(result)
}

#[cfg(test)]
#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
