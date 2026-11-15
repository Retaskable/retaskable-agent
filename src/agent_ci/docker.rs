//! docs/specs/agent-ci.md section 3.6: is Docker usable for CI on this computer? `docker version` from PATH
//! (the program is a parameter so tests can stub it), and `docker run --rm <image> true` for the project's image.
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::time::timeout;

use super::priority::low_priority_tokio;

const VERSION_BUDGET: Duration = Duration::from_secs(20);
/// The first `docker run` pulls the image.
const IMAGE_BUDGET: Duration = Duration::from_secs(15 * 60);

/// The `ci.docker` value, with the reason a person reads when it isn't `ok`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerState {
    Ok,
    Missing(String),
    Stopped(String),
}

impl DockerState {
    /// `"ok" | "missing" | "stopped"`, as the cloud tick carries it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing(_) => "missing",
            Self::Stopped(_) => "stopped",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Ok => None,
            Self::Missing(r) | Self::Stopped(r) => Some(r),
        }
    }
}

const STOPPED: &str = "Docker isn't running: this computer won't run CI";
const MISSING: &str = "Docker isn't installed: this computer won't run CI";
const NO_PERMISSION: &str = "Docker isn't allowed for this user: this computer won't run CI";
const IN_IMAGE: &str = "The agent runs in its own Docker image: this computer's other agent runs CI";

/// Whether this agent runs inside its own Docker image (docker.md): `/.dockerenv`, as the `docker` tag uses.
pub fn in_agent_image() -> bool {
    Path::new("/.dockerenv").exists()
}

fn denied(text: &str) -> bool {
    text.to_lowercase().contains("permission denied")
}

/// `docker version` (client and daemon). Inside the agent's own image it is always `Missing`.
pub async fn probe(program: impl AsRef<OsStr>, in_image: bool) -> DockerState {
    if in_image {
        return DockerState::Missing(IN_IMAGE.into());
    }
    let mut cmd = low_priority_tokio(program);
    cmd.arg("version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            return DockerState::Missing(NO_PERMISSION.into());
        }
        Err(_) => return DockerState::Missing(MISSING.into()),
    };
    let Ok(Ok(out)) = timeout(VERSION_BUDGET, child.wait_with_output()).await else {
        return DockerState::Stopped(STOPPED.into());
    };
    if out.status.success() {
        return DockerState::Ok;
    }
    // `nice` (the Unix priority wrapper) exits 127 when it can't find the program, 126 when it can't run it.
    if out.status.code() == Some(127) {
        return DockerState::Missing(MISSING.into());
    }
    if out.status.code() == Some(126) || denied(&String::from_utf8_lossy(&out.stderr)) {
        return DockerState::Missing(NO_PERMISSION.into());
    }
    DockerState::Stopped(STOPPED.into())
}

/// `docker run --rm <image> true`: the project's image runs (pulled the first time).
pub async fn probe_image(program: impl AsRef<OsStr>, image: &str) -> DockerState {
    let mut cmd = low_priority_tokio(program);
    cmd.args(["run", "--rm", image, "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let fail = |why: &str| {
        DockerState::Stopped(format!(
            "The CI image {image} can't run ({why}): this computer won't run CI"
        ))
    };
    let Ok(child) = cmd.spawn() else {
        return DockerState::Missing(MISSING.into());
    };
    match timeout(IMAGE_BUDGET, child.wait_with_output()).await {
        Ok(Ok(out)) if out.status.success() => DockerState::Ok,
        Ok(Ok(out)) if out.status.code() == Some(127) => DockerState::Missing(MISSING.into()),
        Ok(Ok(out)) => fail(&crate::text::truncate(
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .unwrap_or("failed"),
            120,
        )),
        _ => fail("timed out"),
    }
}

#[cfg(test)]
mod tests;
