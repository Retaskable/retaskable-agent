//! A computer that comes up paused (docs/specs/docker.md section 8): with `RETASKABLE_START_PAUSED=1`, each project this
//! computer has not paused this way before starts with this computer's own pause on (the same pause the site's Pause
//! with `{ computer: true }` and the tray's set, `commands::pause_computer`), until a person resumes it from the site
//! or the settings page. The project is then recorded in `<data folder>/start-paused.json`, so a restart after the
//! resume doesn't pause it again, and a project linked later still starts paused. With the variable unset nothing is
//! read or written (apart from the hold below). If the record can't be written the project is paused anyway and the log says so (the safe way
//! round: it would only pause again at the next start).
//!
//! A project added from the settings page is held the same way whatever the variable says: `hold` writes its id to
//! the file's `hold` list, and the next start (the agent restarts to pick the project up) pauses it and takes it off
//! the list, so it never takes work before a person resumes it (agent-settings.md section 4.7).
use std::path::Path;

use serde_json::{Value, json};

use crate::env::Env;
use crate::fs_util::write_json_atomic;

pub const VAR: &str = "RETASKABLE_START_PAUSED";
const FILE: &str = "start-paused.json";
const HELD_REASON: &str = "Paused when it was added: resume it when it's ready";
const REASON: &str = "Paused at its first start (RETASKABLE_START_PAUSED): resume it when it's ready";

fn recorded(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("projects").cloned())
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default()
}

fn held(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("hold").cloned())
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default()
}

fn write(dir: &Path, seen: &[String], hold: &[String]) -> anyhow::Result<()> {
    let mut file = json!({ "projects": seen });
    if !hold.is_empty() {
        file["hold"] = json!(hold);
    }
    write_json_atomic(&dir.join(FILE), &file, false)
}

/// Marks `project` to start paused at the next start, with the variable set or not. An error means it was not marked,
/// and the caller must not add the project.
pub fn hold(dir: &Path, project: &str) -> anyhow::Result<()> {
    let mut hold = held(dir);
    if !hold.iter().any(|p| p == project) {
        hold.push(project.to_string());
    }
    write(dir, &recorded(dir), &hold)
}

/// Takes `project` off the hold list again (an add that failed after marking it).
pub fn release(dir: &Path, project: &str) {
    let hold: Vec<String> = held(dir).into_iter().filter(|p| p != project).collect();
    if let Err(e) = write(dir, &recorded(dir), &hold) {
        tracing::warn!("Couldn't clear the hold on {project} in {FILE}: {e:#}.");
    }
}

/// Takes `project` out of the file altogether (an unlinked project), from the list of projects paused at their first start
/// and from the hold list.
pub fn forget(dir: &Path, project: &str) -> anyhow::Result<()> {
    let keep = |list: Vec<String>| list.into_iter().filter(|p| p != project).collect::<Vec<_>>();
    if !dir.join(FILE).exists() {
        return Ok(());
    }
    write(dir, &keep(recorded(dir)), &keep(held(dir)))
}

/// Pauses (through `pause(project, reason)`) each of `projects` not yet recorded, when the variable is `1`, and
/// records them. Returns the ones paused.
pub fn apply(dir: &Path, env: &Env, projects: &[String], mut pause: impl FnMut(&str, &str)) -> Vec<String> {
    let mut paused = apply_held(dir, projects, &mut pause);
    paused.extend(apply_env(dir, env, projects, &mut pause));
    paused
}

/// The projects marked by `hold` that are in `projects` are paused and taken off the list; one that isn't there any more
/// is dropped from it.
fn apply_held(dir: &Path, projects: &[String], pause: &mut impl FnMut(&str, &str)) -> Vec<String> {
    let hold = held(dir);
    if hold.is_empty() {
        return Vec::new();
    }
    let now: Vec<String> = hold.into_iter().filter(|p| projects.contains(p)).collect();
    for project in &now {
        pause(project, HELD_REASON);
    }
    // Recorded as paused this way too, so a later start with the variable set doesn't pause it again after a resume.
    let mut seen = recorded(dir);
    seen.extend(now.iter().filter(|p| !seen.contains(p)).cloned().collect::<Vec<_>>());
    if let Err(e) = write(dir, &seen, &[]) {
        tracing::error!("Couldn't clear the hold in {FILE}: {e:#}. These projects will start paused again next time.");
    }
    if !now.is_empty() {
        tracing::info!(
            "Paused {} added from the settings page until resumed: {}.",
            now.len(),
            now.join(", ")
        );
    }
    now
}

fn apply_env(dir: &Path, env: &Env, projects: &[String], pause: &mut impl FnMut(&str, &str)) -> Vec<String> {
    if env.get(VAR).is_none_or(|v| v.trim() != "1") {
        return Vec::new();
    }
    let mut seen = recorded(dir);
    let fresh: Vec<String> = projects.iter().filter(|p| !seen.contains(p)).cloned().collect();
    if fresh.is_empty() {
        return fresh;
    }
    for project in &fresh {
        pause(project, REASON);
    }
    seen.extend(fresh.iter().cloned());
    if let Err(e) = write(dir, &seen, &held(dir)) {
        tracing::error!("Couldn't record {FILE}: {e:#}. These projects will start paused again next time.");
    }
    tracing::info!("{VAR}=1: paused {} until resumed: {}.", fresh.len(), fresh.join(", "));
    fresh
}

#[cfg(test)]
mod tests;
