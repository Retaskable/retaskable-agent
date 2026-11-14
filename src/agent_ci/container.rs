//! docs/specs/agent-ci.md section 3.2 step 2 (Run, no token) and section 3.5 (the log kept): `docker run` of the
//! project's image on a fetched directory, with none of the agent's environment, under a wall-clock ceiling.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tokio::time::timeout;

use super::priority::low_priority_tokio;
use crate::ci_watch::LOG_TAIL_LINES;
use crate::telemetry::now_secs;

/// Wall-clock ceiling of one check (agent-ci.md section 3.2).
pub const CEILING: Duration = Duration::from_secs(45 * 60);
/// How long a log is kept (section 3.5).
pub const LOG_KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Where the directory is mounted inside the container.
pub const WORKDIR: &str = "/workspace";
/// The most of a log read back for its tail.
const TAIL_READ_BYTES: u64 = 1 << 20;

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

/// What `checks.sh` is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    PullRequest,
    Push,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PullRequest => "pull_request",
            Self::Push => "push",
        }
    }
}

/// One check to run.
pub struct Check<'a> {
    /// The `docker` program (injectable for tests).
    pub docker: &'a OsStr,
    pub repo: &'a str,
    pub sha: &'a str,
    /// The fetched directory, mounted as the working directory.
    pub dir: &'a Path,
    pub image: &'a str,
    pub mode: Mode,
    /// Where logs are kept (under the data folder).
    pub log_dir: &'a Path,
    pub ceiling: Duration,
}

/// How a check ended.
#[derive(Debug)]
pub struct Outcome {
    /// `None` when the ceiling killed it (or a signal did).
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub log_path: PathBuf,
    /// The last `LOG_TAIL_LINES` lines of the log.
    pub tail: String,
}

/// `retaskable-ci-<repo>-<short sha>`, with the repo cut to what a container name allows.
pub fn container_name(repo: &str, sha: &str) -> String {
    let repo: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let short: String = sha.chars().take(7).collect();
    format!("retaskable-ci-{repo}-{short}")
}

/// A host directory in the form `docker -v` accepts: on Windows the verbatim `\\?\` prefix is dropped and the
/// separators are `/` (`C:/Users/me/ci`), which Docker Desktop reads; elsewhere the path as is.
pub fn mount_path(dir: &Path) -> String {
    let s = dir.to_string_lossy();
    if cfg!(windows) {
        s.trim_start_matches(r"\\?\").replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// The `docker run` arguments of a check.
pub fn run_args(c: &Check) -> Vec<String> {
    let name = container_name(c.repo, c.sha);
    let strs = [
        "run",
        "--rm",
        "--name",
        &name,
        "--cpu-shares",
        "128",
        "-v",
        &format!("{}:{WORKDIR}", mount_path(c.dir)),
        "-w",
        WORKDIR,
        "-e",
        "CI=true",
        c.image,
        "bash",
        ".agent-ci/checks.sh",
        c.mode.as_str(),
    ];
    strs.iter().map(|s| s.to_string()).collect()
}

/// A low-priority `docker` with its environment cleared to what docker needs (and `DOCKER_*` config).
fn docker_command(docker: &OsStr) -> tokio::process::Command {
    docker_command_from(docker, std::env::vars_os())
}

/// `docker_command` over the given parent environment, so a test needs no process-wide variables.
fn docker_command_from(
    docker: &OsStr,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> tokio::process::Command {
    let mut cmd = low_priority_tokio(docker);
    cmd.env_clear().stdin(Stdio::null()).kill_on_drop(true);
    for (k, v) in vars {
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
    let mut cmd = docker_command(docker);
    cmd.args(["rm", "-f", name]).stdout(Stdio::null()).stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        let _ = timeout(Duration::from_secs(60), child.wait()).await;
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

/// The last `LOG_TAIL_LINES` lines of the file (reading at most its last megabyte).
fn tail_of(path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut f = File::open(path)?;
        let len = f.metadata()?.len();
        f.seek(SeekFrom::Start(len.saturating_sub(TAIL_READ_BYTES)))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        Ok(buf)
    };
    let text = String::from_utf8_lossy(&read().unwrap_or_default()).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n")
}

/// Run the check, streaming stdout and stderr to a log file under `log_dir`; old logs are pruned first.
pub async fn run(c: &Check<'_>) -> Result<Outcome> {
    run_with_env(c, std::env::vars_os().collect::<Vec<_>>()).await
}

/// `run` over the given parent environment (the agent's own, in production).
async fn run_with_env(
    c: &Check<'_>,
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Result<Outcome> {
    std::fs::create_dir_all(c.log_dir).context("creating the CI log folder")?;
    prune_logs(c.log_dir, SystemTime::now());
    let name = container_name(c.repo, c.sha);
    let stamp = now_secs();
    let log_path = c.log_dir.join(format!("{name}-{stamp}.log"));
    let log = File::create(&log_path).context("creating the CI log")?;
    let mut cmd = docker_command_from(c.docker, vars);
    cmd.args(run_args(c)).stdout(log.try_clone()?).stderr(log);
    let mut child = cmd.spawn().context("starting docker run")?;
    let (exit_code, timed_out) = match timeout(c.ceiling, child.wait()).await {
        Ok(status) => (status?.code(), false),
        Err(_) => {
            let _ = child.start_kill();
            remove_stale(c.docker, &name).await;
            let _ = child.wait().await;
            (None, true)
        }
    };
    let tail = tail_of(&log_path);
    Ok(Outcome {
        exit_code,
        timed_out,
        log_path,
        tail,
    })
}

#[cfg(test)]
mod tests;
