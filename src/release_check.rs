//! What a start check needs to run a freshly built release and confirm it behaved, shared by
//! `bin/bundle.rs` and `bin/package.rs` (installers/03-the-bundle-agent-bundle.md section 3.10):
//! CLI argument parsing both share, a scratch home that cleans itself up, a free port, a
//! plain-socket GET loop waiting for the health check to come up, and the two checks a start
//! check always makes once the agent's stopped — nothing on its terminal, and its log says it
//! isn't linked.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::release::{self, Build, Environment};

/// `main`'s usual body for these CLIs: run `body`, printing a one-line error and exiting 1 on
/// failure.
pub fn run_main(body: impl FnOnce() -> Result<(), String>) {
    if let Err(e) = body() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// A CLI flag's value: the token after `name`, if `name` is there.
pub fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `--environment staging|production`, defaulting to Staging: the one flag `bundle` and
/// `package` both take the same way.
pub fn environment_arg(args: &[String]) -> Result<Environment, String> {
    match arg(args, "--environment").as_deref() {
        None | Some("staging") => Ok(Environment::Staging),
        Some("production") => Ok(Environment::Production),
        Some(other) => Err(format!("--environment must be staging or production, not {other}")),
    }
}

/// A scratch home for a start check's installed agent, so it touches neither this machine's own
/// data folder nor its credential store (section 3.10's 2026-10-18 incident, when a scratch run
/// spent and lost this machine's own refresh token by sharing its credential store instead).
/// Removed on drop, on every return path including an early `?`.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    pub fn new(prefix: &str) -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        Ok(Self(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A port nothing is listening on right now, for the start check's own agent (connector/tests.rs's pattern).
pub fn free_port() -> std::io::Result<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()
        .map(|a| a.port())
}

/// A bare GET over a plain socket: the start check has no need for `reqwest`'s TLS or redirects.
pub fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("connecting: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| format!("setting a read timeout: {e}"))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("writing the request: {e}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("reading the response: {e}"))?;
    Ok(response.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
}

/// Polls `GET /api/health` on `port` until it answers `version`, up to 60 seconds.
pub fn wait_for_health(port: u16, version: &str) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut last_err = "no attempt reached it".to_string();
    while std::time::Instant::now() < deadline {
        match http_get(port, "/api/health") {
            Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(answer) if answer.get("version").and_then(|v| v.as_str()) == Some(version) => return Ok(()),
                Ok(answer) => last_err = format!("GET /api/health answered {answer}, not version {version:?}"),
                Err(e) => last_err = format!("GET /api/health answered JSON that didn't parse ({e}): {body}"),
            },
            Err(e) => last_err = e,
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err(format!(
        "127.0.0.1:{port} didn't answer its health within 60 seconds: {last_err}"
    ))
}

/// A start check's agent must log only to its file, never its terminal.
pub fn check_silent_terminal(exe: &Path, output: &Output) -> Result<(), String> {
    if !output.stdout.is_empty() || !output.stderr.is_empty() {
        return Err(format!(
            "{} wrote to its terminal during the start check (a release build must only log to its file):\nstdout: {}\nstderr: {}",
            exe.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// A start check's agent must log the `Links:` line saying it isn't linked — never reaching the
/// install step a start check must never actually exercise.
pub fn check_not_linked_log(build: &Build, scratch: &Path) -> Result<(), String> {
    let scratch = scratch.to_path_buf();
    let scratch_env = move |k: &str| {
        matches!(
            k,
            "HOME" | "USERPROFILE" | "LOCALAPPDATA" | "XDG_DATA_HOME" | "XDG_STATE_HOME"
        )
        .then(|| scratch.to_string_lossy().to_string())
    };
    let log_path = release::logs_dir(build, scratch_env).join("retaskable.log");
    let log = std::fs::read_to_string(&log_path).map_err(|e| format!("reading {}: {e}", log_path.display()))?;
    if !log.lines().any(|l| l.contains("Links:") && l.contains("not linked")) {
        return Err(format!(
            "{} has no Links: line saying it isn't linked:\n{log}",
            log_path.display()
        ));
    }
    Ok(())
}
