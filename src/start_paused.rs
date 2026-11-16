//! A computer that comes up paused (docs/specs/docker.md section 8): with `RETASKABLE_START_PAUSED=1`, each project this
//! computer has not paused this way before starts with this computer's own pause on (the same pause the site's Pause
//! with `{ computer: true }` and the tray's set, `commands::pause_computer`), until a person resumes it from the site
//! or the settings page. The project is then recorded in `<data folder>/start-paused.json`, so a restart after the
//! resume doesn't pause it again, and a project linked later still starts paused. With the variable unset nothing is
//! read or written. If the record can't be written the project is paused anyway and the log says so (the safe way
//! round: it would only pause again at the next start).
use std::path::Path;

use serde_json::{Value, json};

use crate::env::Env;
use crate::fs_util::write_json_atomic;

pub const VAR: &str = "RETASKABLE_START_PAUSED";
const FILE: &str = "start-paused.json";
const REASON: &str = "Paused at its first start (RETASKABLE_START_PAUSED): resume it when it's ready";

fn recorded(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("projects").cloned())
        .and_then(|p| serde_json::from_value(p).ok())
        .unwrap_or_default()
}

/// Pauses (through `pause(project, reason)`) each of `projects` not yet recorded, when the variable is `1`, and
/// records them. Returns the ones paused.
pub fn apply(dir: &Path, env: &Env, projects: &[String], mut pause: impl FnMut(&str, &str)) -> Vec<String> {
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
    if let Err(e) = write_json_atomic(&dir.join(FILE), &json!({ "projects": seen }), false) {
        tracing::error!("Couldn't record {FILE}: {e:#}. These projects will start paused again next time.");
    }
    tracing::info!("{VAR}=1: paused {} until resumed: {}.", fresh.len(), fresh.join(", "));
    fresh
}

#[cfg(test)]
mod tests;
