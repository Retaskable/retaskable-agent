//! Play main (docs/specs/agent/04-architecture.md "Play main", 09-web-ui.md section 9.3): the
//! project's `playCommand` run in the base checkout at `origin/<baseBranch>`, in a visible window
//! and outside any sandbox (the orchestrator's own commands are never sandboxed, 04.2). It holds
//! the base checkout ([`crate::base_hold::hold_base`]) for as long as it runs, so Shoot is refused meanwhile,
//! and it is refused while Shoot holds it. `stop` kills its process tree.

use crate::text::is_blank;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::lanes;
use crate::process_tree::ProcessTree;
use crate::sync::lock;
use crate::view::ProjectView;

/// How long `stop` waits for the process to close.
pub const STOP_LIMIT: Duration = Duration::from_secs(15);

static RUNNING: LazyLock<Mutex<HashMap<String, Arc<ProcessTree>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether Play main is running for `project`.
pub fn running(project: &str) -> bool {
    lock(&RUNNING).contains_key(project)
}

/// Starts Play main: takes the base checkout, moves it to `origin/<baseBranch>`, and runs
/// `playCommand` there on its own thread. `Ok` once the process has started.
pub fn start(project: &str, p: &ProjectView) -> Result<(), String> {
    if is_blank(&p.play_command) {
        return Err("No play command set for this project.".to_string());
    }
    if running(project) {
        return Err("Play main is already running.".to_string());
    }
    let hold = crate::base_hold::hold_base(project, "Play main")?;
    let dir = lanes::prepare_base_checkout(&p.repo_path, &p.worktrees_dir, &p.base_branch)
        .map_err(|e| format!("Couldn't prepare the base checkout: {e:#}"))?;
    let (tree, done) = spawn(&dir, &p.play_command, "play")?;
    lock(&RUNNING).insert(project.to_string(), tree);
    let project = project.to_string();
    std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // The base checkout is let go before the project stops counting as running, so `stop` returning means a
        // `start` can follow at once.
        drop(hold);
        lock(&RUNNING).remove(&project);
    });
    Ok(())
}

/// A game running in an item's lane (`play { lane: k }`): the orchestrator `Machine` keeps it and lets the lane go
/// once [`LaneGame::finished`] or [`LaneGame::stop`].
pub struct LaneGame {
    tree: Arc<ProcessTree>,
    done: Arc<AtomicBool>,
}

impl LaneGame {
    /// Whether the process has ended.
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Kills the process tree, on its own thread so the loop isn't held up; [`LaneGame::finished`] turns true
    /// once it's gone.
    pub fn stop(&self) {
        let tree = self.tree.clone();
        std::thread::spawn(move || {
            tree.kill_and_wait(STOP_LIMIT);
        });
    }
}

/// Runs `command` visibly in `dir` with `RETASKABLE_LANE=<lane>`, unsandboxed.
pub fn start_in_lane(dir: &Path, command: &str, lane: u32) -> Result<LaneGame, String> {
    let (tree, done) = spawn(dir, command, &lane.to_string())?;
    Ok(LaneGame { tree, done })
}

/// Starts `command` in `dir`; the flag is set when it ends.
fn spawn(dir: &Path, command: &str, lane: &str) -> Result<(Arc<ProcessTree>, Arc<AtomicBool>), String> {
    let mut cmd = shell(command);
    cmd.current_dir(dir)
        .env("RETASKABLE_LANE", lane)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ProcessTree::prepare_sync(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start `{command}`: {e}"))?;
    let tree = match ProcessTree::join_sync(&child) {
        Ok(tree) => Arc::new(tree),
        Err(e) => {
            let _ = child.kill();
            return Err(format!("Couldn't track the play process: {e:#}"));
        }
    };
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        flag.store(true, Ordering::SeqCst);
    });
    Ok((tree, done))
}

/// Stops Play main: kills its process tree and waits up to [`STOP_LIMIT`] for it to close.
pub fn stop(project: &str) -> Result<(), String> {
    let tree = lock(&RUNNING).get(project).cloned();
    let Some(tree) = tree else {
        return Err("Play main isn't running.".to_string());
    };
    let began = std::time::Instant::now();
    if tree.kill_and_wait(STOP_LIMIT) {
        while running(project) && began.elapsed() < STOP_LIMIT {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    } else {
        Err("Play main didn't close within 15 seconds.".to_string())
    }
}

/// Restarts Play main: stops it if it is running (nacking as [`stop`] does, and starting nothing, if it didn't
/// close), then [`start`]s it with its own checks and refusals. Not running is not an error.
pub fn restart(project: &str, p: &ProjectView) -> Result<(), String> {
    if running(project) {
        stop(project)?;
    }
    start(project, p)
}

// A window opens for this one (no CREATE_NO_WINDOW), unlike every other child.
#[cfg(windows)]
fn shell(command: &str) -> std::process::Command {
    let mut c = std::process::Command::new("cmd");
    c.args(["/C", command]);
    c
}

#[cfg(not(windows))]
fn shell(command: &str) -> std::process::Command {
    let mut c = std::process::Command::new("sh");
    c.args(["-c", command]);
    c
}

#[cfg(test)]
mod tests;
