//! What unlinking a project takes off this computer (`unlinkProject` in the TS agent's `unlinkProject.ts`, projects.md
//! section 6.6): its entry in `config.json`, its start-paused marker, its lanes (when asked) and its state folder. Nothing on
//! GitHub changes. The loop that held the project runs the first three (`orchestrator::driver::unlink`, after it gave back what
//! it held in the shared backlog), and the command's own thread the last, then restarts the agent.
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::lanes;

/// Takes the project `id` out of `config.json`'s `projects`, keeping everything else in the file.
pub fn remove_from_config(dir: &Path, id: &str) -> Result<()> {
    let mut found = false;
    crate::config::save_machine_settings(dir, |obj| {
        if let Some(Value::Array(list)) = obj.get_mut("projects") {
            let before = list.len();
            list.retain(|p| p.get("id").and_then(Value::as_str) != Some(id));
            found = list.len() != before;
        }
    })
    .context("Couldn't save config.json")?;
    if !found {
        bail!("{id} isn't in config.json (an old single-project file can't have its project unlinked here)");
    }
    Ok(())
}

/// Every lane of the project, any merge worktree left behind and its base and smoke checkouts (`removeWorktrees`), then the
/// worktrees folder itself once it is empty. A folder git no longer tracks is deleted outright (a failed `worktree remove` can
/// leave one behind on Windows). Goes on past one that won't go and says which didn't.
pub fn remove_lanes(repo_path: &Path, worktrees_dir: &Path) -> Result<()> {
    if !worktrees_dir.exists() {
        return Ok(());
    }
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(worktrees_dir).with_context(|| format!("reading {}", worktrees_dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if lanes::parse_lane_name(&name).is_some() || name.starts_with("merge-") {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    dirs.push(lanes::base_checkout_path(worktrees_dir));
    dirs.push(lanes::smoke_path(worktrees_dir));
    let mut failed = Vec::new();
    for dir in dirs.into_iter().filter(|d| d.exists()) {
        let _ = lanes::kill_processes_in(&dir);
        let removed = if lanes::is_worktree(&dir) {
            lanes::remove_worktree(repo_path, &dir)
        } else {
            Ok(())
        };
        // What git no longer tracks, or couldn't take, is deleted by hand.
        if dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&dir)
        {
            failed.push(format!(
                "{}: {}",
                dir.display(),
                removed.err().map_or(e.to_string(), |g| format!("{g:#}"))
            ));
        }
    }
    if std::fs::read_dir(worktrees_dir).is_ok_and(|mut d| d.next().is_none()) {
        let _ = std::fs::remove_dir(worktrees_dir);
    }
    if failed.is_empty() {
        Ok(())
    } else {
        bail!("{}", failed.join("; "))
    }
}

/// The project's state folder (`projects/<id>` under the data folder): its state and transcripts.
pub fn remove_state(dir: &Path, id: &str) -> std::io::Result<()> {
    match std::fs::remove_dir_all(crate::paths::project_dir(dir, id)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        done => done,
    }
}

#[cfg(test)]
mod tests;
