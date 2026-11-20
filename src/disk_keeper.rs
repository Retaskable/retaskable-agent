//! docs/specs/docker.md section 10 (disk hygiene, 2026-11-19), the code agent's side: every few minutes it reads the free
//! space of the data folder and of each project's worktrees folder, holds back new work below the threshold
//! (`crate::disk::held`, which the pool's `health` reports as `refused`) and says what to clean. Pure planning here;
//! the machine's pass (`orchestrator::driver::disk_pass`) does the deleting, in lanes nobody holds.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use std::sync::Arc;

use crate::disk::{self, Change, Held, Limits, Watch};

/// How long a free lane may sit before its `target` and `node_modules` go (`RETASKABLE_LANE_IDLE_HOURS`).
pub const LANE_IDLE: Duration = Duration::from_secs(24 * 3600);
/// How often the routine cleanup runs while the disk is fine.
pub const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// Session transcripts kept: this long, and at most this much, oldest going first.
pub const SESSIONS_KEPT: Duration = Duration::from_secs(30 * 24 * 3600);
pub const SESSIONS_MAX_BYTES: u64 = 5 << 30;
/// While the guard holds, the same but tighter, so a nearly full disk gets room.
pub const SESSIONS_KEPT_LOW: Duration = Duration::from_secs(7 * 24 * 3600);
pub const SESSIONS_MAX_BYTES_LOW: u64 = 1 << 30;

/// What one cleanup does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Free lanes idle at least this long lose `target` and `node_modules` (zero: every free lane).
    pub lane_idle: Duration,
    pub sessions_kept: Duration,
    pub sessions_max_bytes: u64,
    /// The disk is short: this is the immediate cleanup, not the routine one.
    pub urgent: bool,
}

pub struct Keeper {
    data_dir: PathBuf,
    watch: Watch,
    lane_idle: Duration,
    held: Arc<Held>,
    last_sweep: Option<Instant>,
    /// When this agent started: a lane never given back counts as idle since then.
    pub started: Instant,
}

impl Keeper {
    pub fn new(data_dir: PathBuf, limits: Limits, lane_idle: Duration, now: Instant, held: Arc<Held>) -> Self {
        Self {
            data_dir,
            watch: Watch::new(limits),
            lane_idle,
            held,
            last_sweep: None,
            started: now,
        }
    }

    /// From the environment: the thresholds (`crate::disk::Limits::from_env`) and `RETASKABLE_LANE_IDLE_HOURS`.
    pub fn from_env(data_dir: PathBuf, env: &crate::env::Env, now: Instant) -> Self {
        let hours = env
            .get("RETASKABLE_LANE_IDLE_HOURS")
            .and_then(|v| v.trim().parse::<u64>().ok());
        let idle = hours.map_or(LANE_IDLE, |h| Duration::from_secs(h * 3600));
        Self::new(data_dir, Limits::from_env(env), idle, now, disk::held())
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// One look, when one is due: reads each of `watched` (and the data folder) with `free`, updates the guard and
    /// the shared `held` problem, and returns the cleanup to do now. `None` when no look is due, or the disk is fine
    /// and the routine cleanup is not.
    pub fn tick(&mut self, now: Instant, watched: &[PathBuf], free: impl Fn(&Path) -> Option<u64>) -> Option<Plan> {
        let paths: Vec<PathBuf> = std::iter::once(self.data_dir.clone())
            .chain(watched.iter().cloned())
            .collect();
        let change = self.watch.check(now, &paths, free)?;
        self.held.set(self.watch.guard.problem());
        match change {
            Change::Held => tracing::error!("{}", self.watch.guard.problem().unwrap_or_default()),
            Change::Resumed => tracing::info!("Disk space is back: taking new work again"),
            Change::Same => {}
        }
        let holding = self.watch.guard.holding();
        if holding {
            return Some(Plan {
                lane_idle: Duration::ZERO,
                sessions_kept: SESSIONS_KEPT_LOW,
                sessions_max_bytes: SESSIONS_MAX_BYTES_LOW,
                urgent: true,
            });
        }
        if self.last_sweep.is_none_or(|at| now.duration_since(at) >= SWEEP_EVERY) {
            self.last_sweep = Some(now);
            return Some(Plan {
                lane_idle: self.lane_idle,
                sessions_kept: SESSIONS_KEPT,
                sessions_max_bytes: SESSIONS_MAX_BYTES,
                urgent: false,
            });
        }
        None
    }
}

/// The folders holding session transcripts: `<data>/projects/<id>/runs`.
pub fn transcript_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("projects")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join("runs"))
        .filter(|p| p.is_dir())
        .collect()
}

/// Deletes transcripts under the data folder by the plan's age and size. Only files under `projects/*/runs`.
/// Returns the bytes freed.
pub fn prune_transcripts(data_dir: &Path, plan: &Plan, now: SystemTime) -> u64 {
    disk::prune_files(
        &transcript_dirs(data_dir),
        now,
        Some(plan.sessions_kept),
        Some(plan.sessions_max_bytes),
    )
}

#[cfg(test)]
mod tests;
