//! Play main (docs/specs/agent/04-architecture.md "Play main", 09-web-ui.md section 9.3): the
//! project's `playCommand` run in the base checkout at `origin/<baseBranch>`, in a visible window
//! and outside any sandbox (the orchestrator's own commands are never sandboxed, 04.2). It holds
//! the base checkout ([`lanes::hold_base`]) for as long as it runs, so Shoot is refused meanwhile,
//! and it is refused while Shoot holds it. `stop` kills its process tree.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use crate::lanes;
use crate::process_tree::ProcessTree;
use crate::sync::lock;
use crate::view::ProjectView;

/// How long `stop` waits for the process to close.
pub const STOP_LIMIT: Duration = Duration::from_secs(15);

static RUNNING: LazyLock<Mutex<HashMap<String, std::sync::Arc<ProcessTree>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether Play main is running for `project`.
pub fn running(project: &str) -> bool {
    lock(&RUNNING).contains_key(project)
}

/// Starts Play main: takes the base checkout, moves it to `origin/<baseBranch>`, and runs
/// `playCommand` there on its own thread. `Ok` once the process has started.
pub fn start(project: &str, p: &ProjectView) -> Result<(), String> {
    if p.play_command.trim().is_empty() {
        return Err("No play command set for this project.".to_string());
    }
    if running(project) {
        return Err("Play main is already running.".to_string());
    }
    let hold = lanes::hold_base(project, "Play main")?;
    let dir = lanes::prepare_base_checkout(&p.repo_path, &p.worktrees_dir, &p.base_branch)
        .map_err(|e| format!("Couldn't prepare the base checkout: {e:#}"))?;
    let mut command = shell(&p.play_command);
    command
        .current_dir(&dir)
        .env("RETASKABLE_LANE", "play")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ProcessTree::prepare_sync(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Couldn't start `{}`: {e}", p.play_command))?;
    let tree = match ProcessTree::join_sync(&child) {
        Ok(tree) => std::sync::Arc::new(tree),
        Err(e) => {
            let _ = child.kill();
            return Err(format!("Couldn't track the play process: {e:#}"));
        }
    };
    lock(&RUNNING).insert(project.to_string(), tree);
    let project = project.to_string();
    std::thread::spawn(move || {
        let _hold = hold;
        let _ = child.wait();
        lock(&RUNNING).remove(&project);
    });
    Ok(())
}

/// Stops Play main: kills its process tree and waits up to [`STOP_LIMIT`] for it to close.
pub fn stop(project: &str) -> Result<(), String> {
    let tree = lock(&RUNNING).get(project).cloned();
    let Some(tree) = tree else {
        return Err("Play main isn't running.".to_string());
    };
    if tree.kill_and_wait(STOP_LIMIT) {
        Ok(())
    } else {
        Err("Play main didn't close within 15 seconds.".to_string())
    }
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
