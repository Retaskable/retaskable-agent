//! docs/specs/docker.md section 10 (disk hygiene, 2026-11-19): what the agent and the CI runner share to keep a disk
//! from filling. A neutral module: the free space of a folder, the guard's thresholds with a resume margin, and the
//! pure plans for what to delete by age and by size (oldest first). Nothing here deletes outside a folder it is
//! handed, and `prune_files` only removes regular files.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use crate::env::Env;

const GB: u64 = 1 << 30;

/// Free space below which nothing new is taken (`RETASKABLE_DISK_MIN_FREE_GB`).
pub const DEFAULT_MIN_FREE_GB: u64 = 10;
/// Free space above the threshold needed before work resumes (`RETASKABLE_DISK_RESUME_MARGIN_GB`): the guard does not
/// flap while cleanup frees a little at a time.
pub const DEFAULT_MARGIN_GB: u64 = 5;
/// How often the free space is read.
pub const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);
/// How often it is read while the guard holds, so work resumes soon after space is back.
pub const CHECK_EVERY_LOW: Duration = Duration::from_secs(60);

/// The guard's thresholds, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Below this much free, the guard holds.
    pub min_free: u64,
    /// The guard lets go only at `min_free + margin` free.
    pub margin: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            min_free: DEFAULT_MIN_FREE_GB * GB,
            margin: DEFAULT_MARGIN_GB * GB,
        }
    }
}

impl Limits {
    /// The defaults, with `RETASKABLE_DISK_MIN_FREE_GB` and `RETASKABLE_DISK_RESUME_MARGIN_GB` over them (whole GB;
    /// `0` for the first turns the guard off, an unreadable value is ignored).
    pub fn from_env(env: &Env) -> Self {
        let gb = |name: &str, default: u64| {
            env.get(name)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        Self {
            min_free: gb("RETASKABLE_DISK_MIN_FREE_GB", DEFAULT_MIN_FREE_GB) * GB,
            margin: gb("RETASKABLE_DISK_RESUME_MARGIN_GB", DEFAULT_MARGIN_GB) * GB,
        }
    }

    /// Whether the guard is off (a threshold of 0).
    pub fn off(&self) -> bool {
        self.min_free == 0
    }
}

/// The free space of one folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub path: PathBuf,
    pub free: u64,
}

/// What a reading did to the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Same,
    /// Went low: take no new work and clean up now.
    Held,
    /// Back above the threshold plus the margin.
    Resumed,
}

/// The guard's state: holding or not, and the lowest reading it holds for.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    limits: Limits,
    low: Option<Reading>,
}

impl Guard {
    pub fn new(limits: Limits) -> Self {
        Self { limits, low: None }
    }

    /// Takes the readings of every folder watched. The guard holds when any is below `min_free`, and lets go only
    /// when every one is at `min_free + margin` or more. No readings change nothing (an unreadable disk is not a full one).
    pub fn observe(&mut self, readings: &[Reading]) -> Change {
        let Some(lowest) = readings.iter().min_by_key(|r| r.free) else {
            return Change::Same;
        };
        if self.limits.off() {
            return std::mem::take(&mut self.low).map_or(Change::Same, |_| Change::Resumed);
        }
        match &self.low {
            None if lowest.free < self.limits.min_free => {
                self.low = Some(lowest.clone());
                Change::Held
            }
            None => Change::Same,
            Some(_) if lowest.free >= self.limits.min_free.saturating_add(self.limits.margin) => {
                self.low = None;
                Change::Resumed
            }
            Some(_) => {
                // Still holding: say the current lowest.
                self.low = Some(lowest.clone());
                Change::Same
            }
        }
    }

    pub fn holding(&self) -> bool {
        self.low.is_some()
    }

    /// The problem as the board shows it, while holding.
    pub fn problem(&self) -> Option<String> {
        self.low.as_ref().map(|r| problem_text(r.free, &r.path))
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }
}

/// A guard with its schedule: a look at the free space every `CHECK_EVERY` (every `CHECK_EVERY_LOW` while it holds).
#[derive(Debug, Clone)]
pub struct Watch {
    pub guard: Guard,
    next_check: Option<std::time::Instant>,
}

impl Watch {
    pub fn new(limits: Limits) -> Self {
        Self {
            guard: Guard::new(limits),
            next_check: None,
        }
    }

    /// When a look is due (the first one always is), reads each of `paths` with `free` (an unreadable one is left
    /// out), feeds the guard and sets the next look. `None` when no look was due.
    pub fn check(
        &mut self,
        now: std::time::Instant,
        paths: &[PathBuf],
        free: impl Fn(&Path) -> Option<u64>,
    ) -> Option<Change> {
        if self.next_check.is_some_and(|at| now < at) {
            return None;
        }
        let readings: Vec<Reading> = paths
            .iter()
            .filter_map(|p| {
                Some(Reading {
                    path: p.clone(),
                    free: free(p)?,
                })
            })
            .collect();
        let change = self.guard.observe(&readings);
        let every = if self.guard.holding() {
            CHECK_EVERY_LOW
        } else {
            CHECK_EVERY
        };
        self.next_check = Some(now + every);
        Some(change)
    }
}

/// `Disk nearly full: N GB free on <path>; cleaning up, taking no new work`.
pub fn problem_text(free: u64, path: &Path) -> String {
    format!(
        "Disk nearly full: {:.1} GB free on {}; cleaning up, taking no new work",
        free as f64 / GB as f64,
        path.display()
    )
}

/// The agent's guard, read by the pool's health report: `Some(problem)` while the agent holds. A process-wide handle,
/// since the report is built far from where the free space is read.
#[derive(Default)]
pub struct Held(Mutex<Option<String>>);

impl Held {
    pub fn set(&self, problem: Option<String>) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = problem;
    }

    pub fn problem(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

pub fn held() -> Arc<Held> {
    static HELD: OnceLock<Arc<Held>> = OnceLock::new();
    HELD.get_or_init(Arc::default).clone()
}

/// Free bytes available to this user on the disk holding `path` (the nearest folder that exists, when `path` itself
/// doesn't yet). `None` when it can't be read.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut at = path;
    while !at.exists() {
        at = at.parent()?;
    }
    free_bytes_of(at)
}

#[cfg(unix)]
fn free_bytes_of(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a writable statvfs the call fills in.
    if unsafe { libc::statvfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so the struct is initialised.
    let s = unsafe { s.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
}

#[cfg(windows)]
fn free_bytes_of(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and `free` is a writable u64; the other two outputs are optional.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

/// One file as the planners see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

/// The files to delete: every one older than `max_age` before `now`, then, of the rest, the oldest first until the
/// total is at most `max_bytes`. `None` for either leaves that rule off.
pub fn plan_prune(
    files: &[FileInfo],
    now: SystemTime,
    max_age: Option<Duration>,
    max_bytes: Option<u64>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut kept: Vec<&FileInfo> = Vec::new();
    for f in files {
        let old = max_age.is_some_and(|a| now.duration_since(f.modified).is_ok_and(|age| age > a));
        if old {
            out.push(f.path.clone());
        } else {
            kept.push(f);
        }
    }
    if let Some(cap) = max_bytes {
        kept.sort_by_key(|f| f.modified);
        let mut total: u64 = kept.iter().map(|f| f.size).sum();
        for f in kept {
            if total <= cap {
                break;
            }
            total = total.saturating_sub(f.size);
            out.push(f.path.clone());
        }
    }
    out
}

/// Every regular file under `dir` (links are not followed or listed), with its size and time.
pub fn list_files(dir: &Path) -> Vec<FileInfo> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                stack.push(e.path());
            } else if kind.is_file()
                && let Ok(m) = e.metadata()
            {
                out.push(FileInfo {
                    path: e.path(),
                    size: m.len(),
                    modified: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
    }
    out
}

/// Deletes the files `plan_prune` picks among those under `dirs`, and returns the bytes freed. Only regular files
/// are removed, and only under the folders given.
pub fn prune_files(dirs: &[PathBuf], now: SystemTime, max_age: Option<Duration>, max_bytes: Option<u64>) -> u64 {
    let files: Vec<FileInfo> = dirs.iter().flat_map(|d| list_files(d)).collect();
    let mut freed = 0;
    for path in plan_prune(&files, now, max_age, max_bytes) {
        let size = files.iter().find(|f| f.path == path).map_or(0, |f| f.size);
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
        }
    }
    freed
}

#[cfg(test)]
mod tests;
