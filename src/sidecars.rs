//! The code agent starts and keeps the Chat Agent and the CI runner when they are installed beside it (**[Decided
//! 2026-11-16]**, the owner: "Code agent starts them if installed"; docs/specs/programs.md). It is the Rust form of the
//! TypeScript agent's `sidecar.ts`: each is a separate process started by its path, low priority, its output in its own
//! log, started again with a backoff when it exits, and ended with this agent. There is **no channel** between the
//! programs: no RPC, no port, nothing read from the child but its exit. They coordinate through the store and their
//! own status files only, so the CI runner can still run alone on a server. This module names neither program's code
//! (`program_boundaries_tests`): it runs a file.
//!
//! - **Installed** is a file: the `RETASKABLE_CHAT_AGENT_BIN` / `RETASKABLE_CI_BIN` path, else `retaskable-chat-agent` /
//!   `retaskable-ci` next to this executable, else the data folder's `bin/`. Nothing is downloaded.
//! - **Off** with `RETASKABLE_NO_CHAT_AGENT` / `RETASKABLE_NO_CI_RUNNER` (any value, as in the TypeScript agent).
//! - **Already running** (another instance wrote the program's `status.json` in the last 30 seconds, as the CI container
//!   sharing this data folder does) means this one starts nothing, and looks again.
//! - **Wanted** is the program's own plan ([`plan`]): the Chat Agent always; the CI runner when Docker answers, the
//!   slots aren't 0 and a project's base branch has `.agent-ci/`.
//! - The plan is made again every 10 minutes and at each settings save; a changed environment (the CI runner's repos)
//!   starts it again at once.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::Child;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::env::Env;
use crate::low_priority::low_priority_tokio;
use crate::paths;
use crate::process_tree::{self, KILL_LIMIT, ProcessTree};
use crate::program_status::{self, State};

pub mod plan;
#[cfg(all(test, unix))]
mod tests;

/// What differs between the two programs.
#[derive(Debug, Clone)]
pub struct Spec {
    /// Its name in the log: `Chat Agent`, `CI runner`.
    pub label: &'static str,
    /// The binary's file name without `.exe`: `retaskable-chat-agent`, `retaskable-ci`.
    pub exe: &'static str,
    /// Names the binary's path.
    pub bin_var: &'static str,
    /// Any value turns the program off for this computer.
    pub off_var: &'static str,
    /// The program's own folder in the data folder (it keeps `status.json` there).
    pub folder: PathBuf,
    /// Where its output goes.
    pub log: PathBuf,
}

/// The environment a wanted program is started with, besides the data folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Want {
    pub env: Vec<(String, String)>,
}

/// Whether the program is wanted now, and with what. Made again every [`Timing::replan`].
pub type Plan = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<Want>> + Send>> + Send + Sync>;

/// How often things happen; tests make them short.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How often the state is looked at: a program that exited, a status file gone stale, a binary put in place.
    pub tick: Duration,
    /// How often the plan is made again.
    pub replan: Duration,
    /// The first wait before starting it again; it doubles up to `cap`.
    pub first: Duration,
    pub cap: Duration,
    /// A run this long is stable: the next wait starts over at `first`.
    pub stable: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            tick: program_status::INTERVAL / 2,
            replan: Duration::from_secs(10 * 60),
            first: Duration::from_secs(2),
            cap: Duration::from_secs(60),
            stable: Duration::from_secs(60),
        }
    }
}

/// What the next wait is: `previous` doubled to the cap, or the first wait after a run that was stable.
pub fn next_wait(t: &Timing, previous: Duration, ran: Duration) -> Duration {
    if ran >= t.stable || previous.is_zero() {
        t.first
    } else {
        (previous * 2).min(t.cap)
    }
}

/// The binary on this computer: the env override (even when it isn't there), else beside `exe_dir`, else in the data
/// folder's `bin/`.
pub fn find_binary(spec: &Spec, env: &Env, exe_dir: Option<&Path>, data_dir: &Path) -> Option<PathBuf> {
    if let Some(path) = env.get(spec.bin_var).filter(|p| !p.trim().is_empty()) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let file = format!("{}{}", spec.exe, std::env::consts::EXE_SUFFIX);
    exe_dir
        .into_iter()
        .map(Path::to_path_buf)
        .chain([data_dir.join("bin")])
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}

/// Whether the variable is set to anything but nothing, as `!!process.env.X` is.
pub fn switched_off(spec: &Spec, env: &Env) -> bool {
    env.get(spec.off_var).is_some_and(|v| !v.is_empty())
}

/// Another instance is running it: its status file is fresh and isn't the one of a child this agent started.
fn held_by_other(folder: &Path, ours: Option<u32>) -> bool {
    let Some(status) = program_status::read(folder) else {
        return false;
    };
    let fresh = !matches!(
        status.state(program_status::now_ms()),
        State::Stopped(_) | State::NotInstalled
    );
    fresh && Some(status.pid) != ours
}

/// More than this and the log is moved to `<name>.1` (replacing the last) before a start.
const LOG_LIMIT: u64 = 10 * 1024 * 1024;

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(path, old);
    }
    std::fs::OpenOptions::new().create(true).append(true).open(path)
}

struct Running {
    child: Child,
    tree: Arc<ProcessTree>,
    pid: u32,
    key: Want,
    started: Instant,
}

fn launch(spec: &Spec, bin: &Path, data_dir: &Path, want: &Want) -> anyhow::Result<Running> {
    let log = open_log(&spec.log)?;
    let mut cmd = low_priority_tokio(bin);
    cmd.arg("run")
        .env("RETASKABLE_DATA_DIR", data_dir)
        .envs(want.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0000_4000 | 0x0800_0000); // below normal priority, no console window
    ProcessTree::prepare(&mut cmd);
    let child = cmd.spawn()?;
    let tree = ProcessTree::join(&child)?;
    let pid = child.id().unwrap_or_default();
    Ok(Running {
        child,
        tree,
        pid,
        key: want.clone(),
        started: Instant::now(),
    })
}

/// Cuts the child's whole tree (it started git and docker itself) and reaps it.
async fn end(mut r: Running) {
    let tree = r.tree.clone();
    let kill = tokio::task::spawn_blocking(move || tree.kill_and_wait(KILL_LIMIT));
    let (_, _) = tokio::join!(kill, r.child.wait());
    r.tree.finished();
}

async fn exited(running: &mut Option<Running>) -> std::io::Result<std::process::ExitStatus> {
    match running {
        Some(r) => r.child.wait().await,
        None => std::future::pending().await,
    }
}

/// One program's supervisor: a task that keeps it running while it is installed and wanted.
pub struct Sidecar {
    stop: Arc<Notify>,
    replan: Arc<Notify>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Sidecar {
    /// Starts supervising `spec`. Nothing at all when it is switched off.
    pub fn start(
        spec: Spec,
        env: &Env,
        data_dir: PathBuf,
        exe_dir: Option<PathBuf>,
        plan: Plan,
        timing: Timing,
    ) -> Self {
        let this = Self {
            stop: Arc::new(Notify::new()),
            replan: Arc::new(Notify::new()),
            task: Mutex::new(None),
        };
        if switched_off(&spec, env) {
            tracing::info!("{}: off ({} is set).", spec.label, spec.off_var);
            return this;
        }
        let task = tokio::spawn(supervise(
            spec,
            env.clone(),
            data_dir,
            exe_dir,
            plan,
            timing,
            this.stop.clone(),
            this.replan.clone(),
        ));
        *this.task.lock().unwrap() = Some(task);
        this
    }

    /// The plan is made again now: a setting was saved.
    pub fn replan(&self) {
        self.replan.notify_one();
    }

    /// This agent is quitting: the program goes with it, and isn't started again.
    pub async fn stop(&self) {
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            self.stop.notify_one();
            let _ = task.await;
        }
    }
}

/// Says a line only when it differs from the last one said, so a state that lasts isn't repeated each tick.
struct Said(Option<String>, &'static str);

impl Said {
    fn say(&mut self, line: String) {
        if self.0.as_deref() != Some(&line) {
            tracing::info!("{}: {line}", self.1);
            self.0 = Some(line);
        }
    }
    fn forget(&mut self) {
        self.0 = None;
    }
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    spec: Spec,
    env: Env,
    data_dir: PathBuf,
    exe_dir: Option<PathBuf>,
    plan: Plan,
    t: Timing,
    stop: Arc<Notify>,
    replan: Arc<Notify>,
) {
    let mut running: Option<Running> = None;
    let mut want: Option<Want> = None;
    let mut planned: Option<Instant> = None;
    let mut wait = Duration::ZERO;
    let mut not_before: Option<Instant> = None;
    let mut last_pid: Option<u32> = None;
    let mut said = Said(None, spec.label);
    let mut tick = tokio::time::interval(t.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = stop.notified() => break,
            _ = tick.tick() => {}
            _ = replan.notified() => planned = None,
            _ = async {
                match not_before {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
            status = exited(&mut running) => {
                let r = running.take().expect("a child that exited was running");
                r.tree.finished();
                if process_tree::stopping() {
                    break;
                }
                wait = next_wait(&t, wait, r.started.elapsed());
                not_before = Some(Instant::now() + wait);
                let how = status.map_or_else(|e| format!("couldn't be waited for ({e})"), |s| s.to_string());
                tracing::warn!("{}: stopped ({how}); starting it again in {} seconds", spec.label, wait.as_secs_f32());
            }
        }
        if process_tree::stopping() {
            break;
        }
        if planned.is_none_or(|at| at.elapsed() >= t.replan) {
            tokio::select! {
                w = plan() => want = w,
                _ = stop.notified() => break,
            }
            planned = Some(Instant::now());
        }
        match (&want, running.take()) {
            (None, Some(r)) => {
                tracing::info!("{}: no longer wanted, stopping it.", spec.label);
                end(r).await;
                wait = Duration::ZERO;
                not_before = None;
            }
            (Some(w), Some(r)) if r.key != *w => {
                tracing::info!("{}: its settings changed, starting it again.", spec.label);
                end(r).await;
                wait = Duration::ZERO;
                not_before = None;
                running = start(&spec, &env, &data_dir, exe_dir.as_deref(), w, &mut said, &mut last_pid);
            }
            (Some(_), Some(r)) => running = Some(r),
            (Some(w), None) => {
                if not_before.is_some_and(|at| Instant::now() < at) {
                    continue;
                }
                // A missing binary or another instance holding it is looked at again at the next tick.
                running = start(&spec, &env, &data_dir, exe_dir.as_deref(), w, &mut said, &mut last_pid);
            }
            (None, None) => said.forget(),
        }
    }
    if let Some(r) = running.take() {
        end(r).await;
    }
}

/// Starts it when it is installed and no other instance holds it.
fn start(
    spec: &Spec,
    env: &Env,
    data_dir: &Path,
    exe_dir: Option<&Path>,
    want: &Want,
    said: &mut Said,
    last_pid: &mut Option<u32>,
) -> Option<Running> {
    let Some(bin) = find_binary(spec, env, exe_dir, data_dir) else {
        said.say(format!("not installed ({} isn't beside this program).", spec.exe));
        return None;
    };
    if held_by_other(&spec.folder, *last_pid) {
        said.say("another instance is already running it, so this one starts none.".into());
        return None;
    }
    match launch(spec, &bin, data_dir, want) {
        Ok(r) => {
            said.forget();
            *last_pid = Some(r.pid);
            tracing::info!(
                "{}: started {} (pid {}), its output in {}.",
                spec.label,
                bin.display(),
                r.pid,
                spec.log.display()
            );
            Some(r)
        }
        Err(e) => {
            said.say(format!("couldn't start {}: {e}", bin.display()));
            None
        }
    }
}

/// Both supervisors.
pub struct Sidecars {
    chat: Sidecar,
    ci: Sidecar,
}

impl Sidecars {
    /// The settings were saved: the CI runner's repos and slots are looked at now.
    pub fn replan(&self) {
        self.chat.replan();
        self.ci.replan();
    }

    pub async fn stop(&self) {
        tokio::join!(self.chat.stop(), self.ci.stop());
    }
}

/// Starts both. `exe` is this program's own path, where the binaries are looked for first.
pub fn start_all(env: &Env, data_dir: &Path, exe: Option<&Path>) -> Sidecars {
    let exe_dir = exe.and_then(Path::parent).map(Path::to_path_buf);
    let chat = Sidecar::start(
        plan::chat_spec(data_dir),
        env,
        data_dir.to_path_buf(),
        exe_dir.clone(),
        plan::chat_plan(),
        Timing::default(),
    );
    let ci = Sidecar::start(
        plan::ci_spec(data_dir),
        env,
        data_dir.to_path_buf(),
        exe_dir,
        plan::ci_plan(env.clone(), data_dir.to_path_buf(), plan::real_docker()),
        Timing::default(),
    );
    Sidecars { chat, ci }
}

/// The data folder's own parts a spec points at.
pub(crate) fn folders(data_dir: &Path) -> (PathBuf, PathBuf) {
    (paths::chat_dir(data_dir), paths::ci_dir(data_dir))
}
