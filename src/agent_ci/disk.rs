//! docs/specs/docker.md section 10 (disk hygiene, 2026-11-19), the CI runner's side. Each job already removes its own
//! containers and their anonymous volumes (`exec.rs`, `container::remove_job`); this is the rest: what the runner
//! prunes hourly, at start and the moment the disk guard holds. Every docker call is scoped to what the runner made
//! (its `org.retaskable.ci-runner=<computer>` label, its own cache volumes, its own folders under the data folder).
//! The planning is plain code over text and lists, so it is tested without docker.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::container::capture;
use super::settings::Settings;
use super::steps::{self, CACHE_VOLUMES, DEFAULT_IMAGE};
use crate::disk::{self, FileInfo};
use crate::env::Env;

const GB: u64 = 1 << 30;
const LIMIT: Duration = Duration::from_secs(10 * 60);
/// How long a stopped step container of the runner may sit before it is pruned.
pub const CONTAINER_AGE: &str = "1h";
/// How long a dangling image (untagged, used by no container) may sit before it is pruned.
pub const IMAGE_AGE: &str = "24h";
/// How often the routine cleanup runs.
pub const EVERY: Duration = Duration::from_secs(3600);

/// The size caps. A cap of 0 turns that cleanup off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// What Docker's build cache is trimmed to (`AGENT_CI_BUILD_CACHE_GB`).
    pub build_cache: u64,
    /// Each of the runner's cache volumes, npm and cargo (`AGENT_CI_CACHE_GB`).
    pub cache_volume: u64,
    /// All the check logs together (`AGENT_CI_LOG_MB`).
    pub logs: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            build_cache: 10 * GB,
            cache_volume: 5 * GB,
            logs: 500 << 20,
        }
    }
}

impl Caps {
    pub fn from_env(env: &Env) -> Self {
        let n = |name: &str| env.get(name).and_then(|v| v.trim().parse::<u64>().ok());
        let d = Self::default();
        Self {
            build_cache: n("AGENT_CI_BUILD_CACHE_GB").map_or(d.build_cache, |g| g * GB),
            cache_volume: n("AGENT_CI_CACHE_GB").map_or(d.cache_volume, |g| g * GB),
            logs: n("AGENT_CI_LOG_MB").map_or(d.logs, |m| m << 20),
        }
    }

    /// The caps while the disk guard holds: a quarter of each, so room is made quickly.
    pub fn urgent(self) -> Self {
        Self {
            build_cache: self.build_cache / 4,
            cache_volume: self.cache_volume / 4,
            logs: self.logs / 4,
        }
    }
}

/// `docker container prune`, only this computer's runner's containers (by its label) that stopped over an hour ago.
/// Prune removes stopped containers only, never a running one, and the label keeps it to the runner's own.
pub fn container_prune_args(computer: &str) -> Vec<String> {
    let label = steps::runner_label(computer);
    [
        "container",
        "prune",
        "-f",
        "--filter",
        &format!("until={CONTAINER_AGE}"),
        "--filter",
        &format!("label={label}"),
    ]
    .map(String::from)
    .to_vec()
}

/// `docker image prune`: dangling images only (no tag, used by no container), a day old. An image with a name is never
/// pruned, so no image the runner or anyone else uses goes.
pub fn image_prune_args() -> Vec<String> {
    ["image", "prune", "-f", "--filter", &format!("until={IMAGE_AGE}")]
        .map(String::from)
        .to_vec()
}

/// `docker builder prune --keep-storage <cap>`: trims the build cache only over the cap, oldest first. None when
/// the cap is off.
pub fn builder_prune_args(cap: u64) -> Option<Vec<String>> {
    (cap > 0).then(|| {
        ["builder", "prune", "-f", "--keep-storage", &format!("{}MB", cap >> 20)]
            .map(String::from)
            .to_vec()
    })
}

/// The files in a cache volume as `find /v -type f -printf '%T@\t%s\t%p\n'` lists them. A line that doesn't parse is skipped.
pub fn parse_listing(text: &str) -> Vec<FileInfo> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let (time, size, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(FileInfo {
                path: PathBuf::from(path),
                size: size.parse().ok()?,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs_f64(time.parse::<f64>().ok()?.max(0.0)),
            })
        })
        .collect()
}

/// The paths to delete from a cache volume so what is left fits `cap`, oldest first. Only paths inside the volume's
/// mount (`/v/`) are ever named, whatever the listing says.
pub fn plan_cache_trim(listing: &str, cap: u64) -> Vec<String> {
    let files: Vec<FileInfo> = parse_listing(listing)
        .into_iter()
        .filter(|f| f.path.to_string_lossy().starts_with("/v/") && !f.path.to_string_lossy().contains("/../"))
        .collect();
    disk::plan_prune(&files, SystemTime::now(), None, Some(cap))
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// The runner's bare mirrors (`<ci folder>/mirrors/<owner>-<repo>.git`) that no configured repo uses any more: one
/// cached clone is kept per repo, so a repo taken out of the settings leaves its mirror to be removed.
pub fn stale_mirrors(mirrors_dir: &Path, cfg: &Settings) -> Vec<PathBuf> {
    let keep: Vec<PathBuf> = cfg
        .repos
        .iter()
        .map(|r| super::settings::mirror_dir(&cfg.data_dir, r))
        .collect();
    let Ok(entries) = std::fs::read_dir(mirrors_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_some_and(|e| e == "git") && !keep.contains(p))
        .collect()
}

/// The folders of the check logs: `<ci folder>/projects/<owner>-<repo>/ci-logs`.
pub fn log_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("projects")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join("ci-logs"))
        .filter(|p| p.is_dir())
        .collect()
}

/// Prunes the logs by age (a day while urgent, else the week they are kept) and then by the total size, oldest first.
pub fn prune_logs(data_dir: &Path, caps: &Caps, urgent: bool, now: SystemTime) -> u64 {
    let age = if urgent {
        Duration::from_secs(86400)
    } else {
        super::container::LOG_KEPT
    };
    disk::prune_files(
        &log_dirs(data_dir),
        now,
        Some(age),
        (caps.logs > 0).then_some(caps.logs),
    )
}

/// One docker `prune` call, said in the log when it freed something.
async fn prune(docker: &OsStr, what: &str, args: Vec<String>) {
    if let Some(out) = capture(docker, &args, None, LIMIT).await
        && let Some(line) = out.lines().rev().find(|l| l.contains("reclaimed"))
    {
        tracing::info!("disk cleanup, {what}: {}", line.trim());
    }
}

/// Trims one of the runner's cache volumes to `cap`, oldest files first. Needs the default image already here (a
/// pull would use the disk it is trying to free). Only called with no check running.
async fn trim_volume(docker: &OsStr, computer: &str, volume: &str, cap: u64) {
    let have = capture(
        docker,
        &["image".into(), "inspect".into(), DEFAULT_IMAGE.into()],
        None,
        LIMIT,
    )
    .await;
    if have.is_none() {
        return;
    }
    let label = steps::runner_label(computer);
    let Some(listing) = capture(docker, &list_args(&label, volume), None, LIMIT).await else {
        return;
    };
    let gone = plan_cache_trim(&listing, cap);
    if gone.is_empty() {
        return;
    }
    let n = gone.len();
    if capture(docker, &delete_args(&label, volume), Some(gone.join("\n")), LIMIT)
        .await
        .is_some()
    {
        tracing::info!(
            "disk cleanup: trimmed {n} old file(s) from the {volume} volume to its {} GB cap",
            cap / GB
        );
    }
}

/// The delete: paths on stdin, one per line, removed inside the volume's mount.
pub fn delete_args(label: &str, volume: &str) -> Vec<String> {
    [
        "run",
        "--rm",
        "-i",
        "--label",
        label,
        "-v",
        &format!("{volume}:/v"),
        DEFAULT_IMAGE,
        "sh",
        "-c",
        "while IFS= read -r f; do rm -f -- \"$f\"; done",
    ]
    .map(String::from)
    .to_vec()
}

/// The listing of a cache volume, mounted read-only.
pub fn list_args(label: &str, volume: &str) -> Vec<String> {
    [
        "run",
        "--rm",
        "--label",
        label,
        "-v",
        &format!("{volume}:/v:ro"),
        DEFAULT_IMAGE,
        "find",
        "/v",
        "-type",
        "f",
        "-printf",
        "%T@\\t%s\\t%p\\n",
    ]
    .map(String::from)
    .to_vec()
}

/// The routine or urgent cleanup. `checks_running`: a check is running, so the cache volumes it mounts are left
/// alone. Nothing here touches a container, image or volume the runner did not make, or anything outside its folder.
pub async fn maintain(docker: &OsStr, cfg: &Settings, caps: &Caps, urgent: bool, checks_running: bool) {
    let caps = if urgent { caps.urgent() } else { *caps };
    prune(docker, "stopped step containers", container_prune_args(&cfg.computer)).await;
    prune(docker, "dangling images", image_prune_args()).await;
    if let Some(args) = builder_prune_args(caps.build_cache) {
        prune(docker, "build cache", args).await;
    }
    if !checks_running && caps.cache_volume > 0 {
        for volume in CACHE_VOLUMES {
            trim_volume(docker, &cfg.computer, volume, caps.cache_volume).await;
        }
    }
    let data_dir = cfg.data_dir.clone();
    let mirrors = stale_mirrors(&data_dir.join("mirrors"), cfg);
    let freed = tokio::task::spawn_blocking(move || {
        for m in mirrors {
            let _ = std::fs::remove_dir_all(m);
        }
        prune_logs(&data_dir, &caps, urgent, SystemTime::now())
    })
    .await
    .unwrap_or(0);
    if freed > 0 {
        tracing::info!("disk cleanup: removed {:.1} MB of old check logs", freed as f64 / 1e6);
    }
}

#[cfg(test)]
mod tests;
