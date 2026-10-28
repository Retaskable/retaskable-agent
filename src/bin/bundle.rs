//! `cargo run --bin bundle -- [--environment staging|production] [--out <folder>] [--version <n.n.n>]`
//! (installers/03-the-bundle-agent-bundle.md sections 3.9 and 3.10), by default this machine's target
//! and Staging, into `dist/agent/<environment>-<target>/`: builds `retaskable`'s release executable,
//! writes `build.json` beside it, checks that `--version` prints what was written, then runs the
//! start check (section 3.10): starts it as an installed agent, with a scratch home so it touches
//! neither this machine's own data folder nor its credential store.
//!
//! Only the machine's own target is built here (`release::target_slug`); the TypeScript agent's
//! `npm run bundle` can also cross-build the other three, which this doesn't do yet. Nor does this
//! check the Windows executable's PE subsystem (section 3.2/3.9): it isn't set to hide the console yet,
//! which needs the tray this port doesn't have.
use std::path::{Path, PathBuf};
use std::process::Command;

use retaskable_agent::release::{self, Environment};

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let environment = match arg(&args, "--environment").as_deref() {
        None | Some("staging") => Environment::Staging,
        Some("production") => Environment::Production,
        Some(other) => return Err(format!("--environment must be staging or production, not {other}")),
    };
    let target = release::target_slug(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        format!(
            "this machine ({}-{}) isn't one of the bundle's targets yet",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let version = match arg(&args, "--version") {
        Some(v) if is_dotted_version(&v) => v,
        Some(other) => return Err(format!("--version must be dotted numbers (n.n.n), not {other}")),
        None if args.iter().any(|a| a == "--version") => {
            return Err("--version needs a value (dotted numbers, n.n.n)".to_string());
        }
        None => env!("CARGO_PKG_VERSION").to_string(),
    };
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let commit = git_short_head(&manifest_dir)?;
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("target"));
    let out = arg(&args, "--out").map(PathBuf::from).unwrap_or_else(|| {
        manifest_dir
            .join("dist/agent")
            .join(format!("{}-{target}", environment.name()))
    });

    println!("Building {target} ({}) into {}…", environment.name(), out.display());
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "retaskable"])
        .current_dir(&manifest_dir)
        .status()
        .map_err(|e| format!("running cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build --release failed".to_string());
    }

    std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    let exe_name = if cfg!(windows) { "retaskable.exe" } else { "retaskable" };
    let built = target_dir.join("release").join(exe_name);
    let placed = out.join(exe_name);
    std::fs::copy(&built, &placed).map_err(|e| format!("copying {} to {}: {e}", built.display(), placed.display()))?;

    let build_json = serde_json::json!({
        "os": target,
        "environment": environment.name(),
        "version": version,
        "commit": commit,
    });
    let build_json_path = out.join("build.json");
    let text = serde_json::to_string_pretty(&build_json).map_err(|e| format!("writing build.json: {e}"))?;
    std::fs::write(&build_json_path, text + "\n").map_err(|e| format!("writing {}: {e}", build_json_path.display()))?;

    println!("Checking {}…", placed.display());
    let output = Command::new(&placed)
        .arg("--version")
        .output()
        .map_err(|e| format!("running {}: {e}", placed.display()))?;
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if printed != version {
        return Err(format!(
            "{} --version printed {printed:?}, not build.json's {version:?}",
            placed.display()
        ));
    }

    println!("Starting {} for its start check…", placed.display());
    let build = release::Build {
        os: target.to_string(),
        environment,
        version: version.clone(),
        commit: commit.clone(),
    };
    start_check(&placed, &build)?;

    println!("Built {target} {version} ({commit}) at {}", out.display());
    Ok(())
}

/// Section 3.10's start check: starts `placed` as an installed agent is started — a scratch home, so its
/// folders are new, and its secrets go to a file there rather than this machine's own credential store
/// (section 3.10's 2026-10-18 incident, when a scratch run spent and lost this machine's own refresh token
/// by sharing its credential store instead). Fails unless, within 60 seconds, `GET /api/health` answers
/// `build`'s version, the log has the `Links:` line saying not linked, and nothing reached the terminal.
fn start_check(placed: &Path, build: &release::Build) -> Result<(), String> {
    let scratch = std::env::temp_dir().join(format!(
        "retaskable-bundle-check-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("creating {}: {e}", scratch.display()))?;
    let cleanup = |result: Result<(), String>| {
        let _ = std::fs::remove_dir_all(&scratch);
        result
    };
    let port = match free_port() {
        Ok(port) => port,
        Err(e) => return cleanup(Err(format!("finding a free port for the start check: {e}"))),
    };

    let mut child = match Command::new(placed)
        .env("HOME", &scratch)
        .env("USERPROFILE", &scratch)
        .env("LOCALAPPDATA", &scratch)
        .env("XDG_DATA_HOME", &scratch)
        .env("XDG_STATE_HOME", &scratch)
        .env("RETASKABLE_PORT", port.to_string())
        .env("RETASKABLE_CREDENTIALS_FILE", scratch.join("credentials.json"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return cleanup(Err(format!("running {}: {e}", placed.display()))),
    };

    let health = wait_for_health(port, &build.version);
    let _ = child.kill();
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(e) => return cleanup(Err(format!("waiting for {}: {e}", placed.display()))),
    };
    if let Err(e) = health {
        return cleanup(Err(e));
    }
    if !output.stdout.is_empty() || !output.stderr.is_empty() {
        return cleanup(Err(format!(
            "{} wrote to its terminal during the start check (a release build must only log to its file):\nstdout: {}\nstderr: {}",
            placed.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let scratch_env = {
        let scratch = scratch.clone();
        move |k: &str| {
            matches!(
                k,
                "HOME" | "USERPROFILE" | "LOCALAPPDATA" | "XDG_DATA_HOME" | "XDG_STATE_HOME"
            )
            .then(|| scratch.to_string_lossy().to_string())
        }
    };
    let log_path = release::logs_dir(build, scratch_env).join("retaskable.log");
    let log = match std::fs::read_to_string(&log_path) {
        Ok(log) => log,
        Err(e) => return cleanup(Err(format!("reading {}: {e}", log_path.display()))),
    };
    if !log.lines().any(|l| l.contains("Links:") && l.contains("not linked")) {
        return cleanup(Err(format!(
            "{} has no Links: line saying it isn't linked:\n{log}",
            log_path.display()
        )));
    }

    cleanup(Ok(()))
}

/// Polls `GET /api/health` on `port` until it answers `version`, up to 60 seconds.
fn wait_for_health(port: u16, version: &str) -> Result<(), String> {
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

/// A bare GET over a plain socket: the start check has no need for `reqwest`'s TLS or redirects.
fn http_get(port: u16, path: &str) -> Result<String, String> {
    use std::io::{Read, Write};
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

/// A port nothing is listening on right now, for the start check's own agent (connector/tests.rs's pattern).
fn free_port() -> std::io::Result<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()
        .map(|a| a.port())
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn git_short_head(dir: &std::path::Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("running git rev-parse --short HEAD: {e}"))?;
    if !output.status.success() {
        return Err("git rev-parse --short HEAD failed".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Section 3.16: `--version` must be dotted numbers (`n.n.n`), refused otherwise.
fn is_dotted_version(v: &str) -> bool {
    !v.is_empty()
        && v.split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
