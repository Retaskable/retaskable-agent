//! docs/specs/agent-ci.md section 3.2 (the steps) and section 3.5 (the log kept): `docker run` of one step with none
//! of the agent's environment (the fetch gets the token as `T`, nothing else does), under a wall-clock ceiling, its
//! output kept in the check's log.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tokio::time::timeout;

use crate::low_priority::low_priority_tokio;

/// Wall-clock ceiling of one check (agent-ci.md section 3.2).
pub const CEILING: Duration = Duration::from_secs(45 * 60);
/// How long a log is kept (section 3.5).
pub const LOG_KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The most of a log read back.
const TAIL_READ_BYTES: u64 = 8 << 20;

/// The environment variables `docker` itself needs; nothing else of the agent's reaches it or the container.
const DOCKER_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "SystemRoot",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "TEMP",
    "TMP",
    "XDG_RUNTIME_DIR",
];

/// A low-priority `docker` with its environment cleared to what docker needs (and `DOCKER_*` config).
fn docker_command(docker: &OsStr) -> tokio::process::Command {
    docker_command_from(docker, std::env::vars_os())
}

/// `docker_command` over the given parent environment (the agent's own, in production).
fn docker_command_from(
    docker: &OsStr,
    parent: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> tokio::process::Command {
    let mut cmd = low_priority_tokio(docker);
    cmd.env_clear().stdin(Stdio::null()).kill_on_drop(true);
    for (k, v) in parent {
        let key = k.to_string_lossy();
        let wanted =
            DOCKER_ENV.iter().any(|w| key.eq_ignore_ascii_case(w)) || key.to_ascii_uppercase().starts_with("DOCKER_");
        if wanted {
            cmd.env(k, v);
        }
    }
    cmd
}

/// `docker rm -f <name>`: removes a container still running from a cut check. Nothing there is not an error.
pub async fn remove_stale(docker: &OsStr, name: &str) {
    remove(docker, &[name.to_string()]).await;
}

async fn remove(docker: &OsStr, names: &[String]) {
    let mut cmd = docker_command(docker);
    cmd.args(["rm", "-f"])
        .args(names)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        let _ = timeout(Duration::from_secs(60), child.wait()).await;
    }
}

/// At start, removes this runner's own leftover step containers (a killed `docker run` leaves its container going):
/// every container whose name starts with `prefix` (`steps::name_prefix`). Docker not answering is not an error:
/// the jobs say so.
pub async fn remove_leftovers(docker: &OsStr, prefix: &str) {
    let mut cmd = docker_command(docker);
    cmd.args(["ps", "-aq", "--filter", &format!("name=^{prefix}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(Ok(out)) = timeout(Duration::from_secs(60), cmd.output()).await else {
        return;
    };
    let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(String::from)
        .collect();
    if !ids.is_empty() {
        remove(docker, &ids).await;
    }
}

/// Delete the logs in `dir` last written more than `LOG_KEPT` before `now`.
pub fn prune_logs(dir: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > LOG_KEPT);
        if old && e.path().is_file() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The log's text, at most its last `TAIL_READ_BYTES`.
pub fn read_log(path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut f = File::open(path)?;
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(TAIL_READ_BYTES)))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(buf)
    };
    String::from_utf8_lossy(&read().unwrap_or_default()).into_owned()
}

/// The log of one check: `<short sha>-<event>.log`, started afresh with `header`; old logs are pruned first.
pub fn start_log(log_dir: &Path, sha: &str, event: &str, header: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(log_dir).context("creating the CI log folder")?;
    prune_logs(log_dir, SystemTime::now());
    let short: String = sha.chars().take(12).collect();
    let path = log_dir.join(format!("{short}-{event}.log"));
    std::fs::write(
        &path,
        format!(
            "{header}
"
        ),
    )
    .context("creating the CI log")?;
    Ok(path)
}

/// One `docker run` of a step.
pub struct Step<'a> {
    /// The `docker` program (injectable for tests).
    pub docker: &'a OsStr,
    /// The container's name, so a cut run's container can be removed.
    pub name: &'a str,
    pub args: Vec<String>,
    /// Put in `docker`'s own environment as `T`, for a step that takes `-e T`.
    pub token: Option<&'a str>,
    /// Stdout and stderr are appended here.
    pub log: &'a Path,
    pub ceiling: Duration,
}

/// How a step ended.
#[derive(Debug)]
pub struct Ended {
    /// `None` when the ceiling killed it (or a signal did).
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

/// Runs the step under its ceiling, appending its output to the log.
pub async fn run(s: &Step<'_>) -> Result<Ended> {
    let log = File::options()
        .append(true)
        .create(true)
        .open(s.log)
        .context("opening the CI log")?;
    let mut cmd = docker_command(s.docker);
    cmd.args(&s.args).stdout(log.try_clone()?).stderr(log);
    if let Some(t) = s.token {
        cmd.env("T", t);
    }
    let mut child = cmd.spawn().context("starting docker run")?;
    let (exit_code, timed_out) = match timeout(s.ceiling, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => {
            let _ = child.start_kill();
            remove_stale(s.docker, s.name).await;
            let _ = child.wait().await;
            (None, true)
        }
    };
    Ok(Ended { exit_code, timed_out })
}

#[cfg(test)]
mod tests;
