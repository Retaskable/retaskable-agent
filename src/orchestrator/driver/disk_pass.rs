//! docs/specs/docker.md section 10 (disk hygiene, 2026-11-19): the machine's share of keeping the disk from filling.
//! Each pass asks the [`Keeper`](crate::disk_keeper::Keeper) whether a look at the free space is due; the cleanup it
//! plans runs here, in the lanes nobody holds (a held lane is never touched), and over the session transcripts.
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use super::machine::Machine;
use crate::disk_keeper::{self, Keeper};

impl Machine {
    /// Reads the free space when due, holds or lets go of new work (through `crate::disk::held`), and cleans.
    pub(super) async fn disk_pass(&mut self) {
        let now = Instant::now();
        let watched: Vec<PathBuf> = self
            .projects
            .iter()
            .map(|lp| lp.project.worktrees_dir.as_str())
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .collect();
        let Some(keeper) = self.disk.as_mut() else { return };
        let Some(plan) = keeper.tick(now, &watched, crate::disk::free_bytes) else {
            return;
        };
        let started = keeper.started;
        let data_dir = keeper.data_dir().to_path_buf();
        // Lanes nobody holds that have sat idle (every free one when urgent), in each project's worktrees folder.
        let lanes = self.pool.idle_lanes(started, now, plan.lane_idle);
        let dirs: Vec<PathBuf> = watched
            .iter()
            .flat_map(|wd| lanes.iter().map(|k| crate::lanes::lane_path(wd, *k)))
            .collect();
        let cleaned = tokio::task::spawn_blocking(move || {
            let lane_bytes: u64 = dirs.iter().map(|d| crate::lanes::clear_build_output(d)).sum();
            let transcript_bytes = disk_keeper::prune_transcripts(&data_dir, &plan, SystemTime::now());
            (lane_bytes, transcript_bytes)
        })
        .await;
        match cleaned {
            Ok((lane_bytes, transcript_bytes)) if lane_bytes + transcript_bytes > 0 => tracing::info!(
                "disk cleanup{}: cleared {:.1} GB of lane build output and {:.1} GB of old session transcripts",
                if plan.urgent { " (disk nearly full)" } else { "" },
                lane_bytes as f64 / 1e9,
                transcript_bytes as f64 / 1e9
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!("disk cleanup failed: {e}"),
        }
    }

    /// Gives the machine the disk guard (the agent's own startup; a test's machine has none, so none is held back).
    pub fn with_disk(mut self, keeper: Keeper) -> Self {
        self.disk = Some(keeper);
        self
    }
}
