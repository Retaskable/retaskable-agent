//! What a start check needs to run a freshly built release and confirm it behaved, shared by
//! `bin/bundle.rs` and `bin/package.rs` (installers/03-the-bundle-agent-bundle.md section 3.10):
//! CLI argument parsing both share, a scratch home that cleans itself up, a free port, a
//! plain-socket GET loop waiting for the health check to come up, and the two checks a start
//! check always makes once the agent's stopped — nothing on its terminal, and its log says it
//! isn't linked.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::env::Env;
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
            crate::telemetry::events::now_nanos()
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
    let scratch_env = Env::new(move |k: &str| {
        matches!(
            k,
            "HOME" | "USERPROFILE" | "LOCALAPPDATA" | "XDG_DATA_HOME" | "XDG_STATE_HOME"
        )
        .then(|| scratch.to_string_lossy().to_string())
    });
    let log_path = release::logs_dir(build, &scratch_env).join("retaskable.log");
    let log = std::fs::read_to_string(&log_path).map_err(|e| format!("reading {}: {e}", log_path.display()))?;
    if !log.lines().any(|l| l.contains("Links:") && l.contains("not linked")) {
        return Err(format!(
            "{} has no Links: line saying it isn't linked:\n{log}",
            log_path.display()
        ));
    }
    Ok(())
}

/// Section 3.10's start check, shared by `bundle` and `package`: starts `exe` with a scratch home
/// (so its folders are new and its secrets go to a file there), then fails unless, within 60
/// seconds, `GET /api/health` answers `build`'s version, nothing reached the terminal and the log
/// has the `Links:` line saying not linked. `extra_env` is set too (an AppImage's
/// `APPIMAGE_EXTRACT_AND_RUN`). With `own_group` (Unix) the child leads its own process group and
/// the whole group is stopped, for an AppImage whose runtime runs the agent as its child; else just
/// the child is.
pub fn start_check(
    exe: &Path,
    build: &Build,
    prefix: &str,
    extra_env: &[(&str, &str)],
    own_group: bool,
) -> Result<(), String> {
    let scratch = ScratchDir::new(prefix)?;
    let port = free_port().map_err(|e| format!("finding a free port for the start check: {e}"))?;

    let mut cmd = std::process::Command::new(exe);
    for key in ["HOME", "USERPROFILE", "LOCALAPPDATA", "XDG_DATA_HOME", "XDG_STATE_HOME"] {
        cmd.env(key, scratch.path());
    }
    cmd.env("RETASKABLE_PORT", port.to_string())
        .env("RETASKABLE_CREDENTIALS_FILE", scratch.path().join("credentials.json"))
        .envs(extra_env.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    if own_group {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("running {}: {e}", exe.display()))?;

    let health = wait_for_health(port, &build.version);
    stop(&mut child, own_group);
    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting for {}: {e}", exe.display()))?;
    health?;
    check_silent_terminal(exe, &output)?;
    check_not_linked_log(build, scratch.path())
}

#[cfg(unix)]
fn stop(child: &mut std::process::Child, own_group: bool) {
    if own_group {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{}", child.id())])
            .status();
    } else {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn stop(child: &mut std::process::Child, _own_group: bool) {
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn build() -> Build {
        Build {
            os: "linux".into(),
            environment: Environment::Staging,
            version: "1.2.3".into(),
            commit: "abc".into(),
        }
    }

    /// Serves a canned HTTP response to every connection on a free port.
    fn serve_once(body: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // Read the whole request first: closing with it unread would reset the connection.
                let mut request = Vec::new();
                let mut buf = [0u8; 256];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}");
            }
        });
        port
    }

    #[test]
    fn arg_returns_the_token_after_the_flag() {
        let a = args(&["--out", "dir", "--x"]);
        assert_eq!(arg(&a, "--out").as_deref(), Some("dir"));
        assert_eq!(arg(&a, "--missing"), None);
        // A flag in last position has no value.
        assert_eq!(arg(&a, "--x"), None);
    }

    #[test]
    fn environment_arg_defaults_to_staging_and_rejects_unknown() {
        assert_eq!(environment_arg(&[]).unwrap(), Environment::Staging);
        assert_eq!(
            environment_arg(&args(&["--environment", "staging"])).unwrap(),
            Environment::Staging
        );
        assert_eq!(
            environment_arg(&args(&["--environment", "production"])).unwrap(),
            Environment::Production
        );
        let err = environment_arg(&args(&["--environment", "prod"])).unwrap_err();
        assert!(err.contains("staging or production, not prod"), "{err}");
    }

    #[test]
    fn scratch_dir_exists_then_is_removed_on_drop() {
        let scratch = ScratchDir::new("rc-test").unwrap();
        let path = scratch.path().to_path_buf();
        std::fs::write(path.join("f"), "x").unwrap();
        assert!(path.is_dir());
        drop(scratch);
        assert!(!path.exists());
    }

    #[test]
    fn scratch_dirs_are_distinct() {
        let a = ScratchDir::new("rc-test").unwrap();
        let b = ScratchDir::new("rc-test").unwrap();
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn free_port_is_bindable() {
        let port = free_port().unwrap();
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn http_get_returns_only_the_body() {
        let port = serve_once("hello");
        assert_eq!(http_get(port, "/x").unwrap(), "hello");
    }

    #[test]
    fn http_get_errors_when_nothing_listens() {
        let port = free_port().unwrap();
        assert!(http_get(port, "/").unwrap_err().starts_with("connecting"));
    }

    #[test]
    fn wait_for_health_accepts_the_matching_version() {
        let port = serve_once(r#"{"version":"1.2.3"}"#);
        wait_for_health(port, "1.2.3").unwrap();
    }

    #[test]
    fn silent_terminal_passes_on_empty_output_and_fails_otherwise() {
        let exe = Path::new("agent");
        let out = |stdout: &str, stderr: &str| Output {
            status: std::process::ExitStatus::default(),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        assert!(check_silent_terminal(exe, &out("", "")).is_ok());
        let err = check_silent_terminal(exe, &out("hi", "")).unwrap_err();
        assert!(err.contains("stdout: hi"), "{err}");
        let err = check_silent_terminal(exe, &out("", "oops")).unwrap_err();
        assert!(err.contains("stderr: oops"), "{err}");
    }

    #[test]
    fn not_linked_log_needs_a_links_line_saying_not_linked() {
        let scratch = ScratchDir::new("rc-test").unwrap();
        let b = build();
        let p = scratch.path().to_path_buf();
        let env = Env::new(move |k: &str| {
            matches!(
                k,
                "HOME" | "USERPROFILE" | "LOCALAPPDATA" | "XDG_DATA_HOME" | "XDG_STATE_HOME"
            )
            .then(|| p.to_string_lossy().to_string())
        });
        let dir = release::logs_dir(&b, &env);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("retaskable.log");

        // No log file at all.
        assert!(
            check_not_linked_log(&b, scratch.path())
                .unwrap_err()
                .contains("reading")
        );

        // A Links: line that doesn't say "not linked", and a "not linked" line without Links:.
        std::fs::write(&log, "Links: linked\nsomething not linked\n").unwrap();
        assert!(
            check_not_linked_log(&b, scratch.path())
                .unwrap_err()
                .contains("no Links: line")
        );

        std::fs::write(&log, "boot\nLinks: not linked\n").unwrap();
        check_not_linked_log(&b, scratch.path()).unwrap();
    }
}
