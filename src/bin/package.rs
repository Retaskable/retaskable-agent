//! `cargo run --bin package -- [--environment staging|production] [--bundle <folder>]
//! [--out <folder>] [--cache <folder>]` (installers/03-the-bundle-agent-bundle.md section 3.15,
//! installers/06-macos-and-linux.md section 6.2): packages `bundle`'s already-built Linux output
//! (`cargo run --bin bundle` first) into an AppImage under `out`. Needs `mksquashfs`
//! (squashfs-tools), so it builds only on Linux or macOS; the resulting AppImage is only run as a
//! start check when this machine is itself `linux-x64`, the same scratch-home approach as
//! `bundle`'s own check.
use std::path::{Path, PathBuf};
use std::process::Command;

use retaskable_agent::package;
use retaskable_agent::release::{self, Environment};
use retaskable_agent::release_check::{free_port, wait_for_health};

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
    let target = arg(&args, "--target").unwrap_or_else(|| "linux-x64".to_string());
    if target != "linux-x64" {
        return Err(format!(
            "package only builds the Linux AppImage (--target must be linux-x64, not {target}): macOS's package \
             is the desktop app's now (installers/06-macos-and-linux.md section 6.1) and Windows's installer is \
             a step of its own"
        ));
    }
    if !cfg!(unix) {
        return Err(
            "an AppImage needs mksquashfs and a Unix-like filesystem for its symlinks: run this on Linux or \
             macOS, not Windows"
                .to_string(),
        );
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bundle = arg(&args, "--bundle").map(PathBuf::from).unwrap_or_else(|| {
        manifest_dir
            .join("dist/agent")
            .join(format!("{}-{target}", environment.name()))
    });
    let out = arg(&args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("dist/agent/packages"));
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("target"));
    let cache = arg(&args, "--cache")
        .map(PathBuf::from)
        .unwrap_or_else(|| target_dir.join("retaskable-cache"));

    let build = release::load_build(&bundle.join("retaskable"))
        .map_err(|e| format!("reading {}'s build.json: {e}", bundle.display()))?
        .ok_or_else(|| format!("{} has no build.json yet: run bundle first", bundle.display()))?;

    println!("Packaging {} ({})…", bundle.display(), build.os);
    let (file, sha256) = package::make_package(&bundle, &build, &out, &cache).map_err(|e| format!("{e:#}"))?;
    println!("Packaged {} (SHA-256 {sha256})", file.display());

    let host_target = release::target_slug(std::env::consts::OS, std::env::consts::ARCH);
    if host_target == Some("linux-x64") {
        println!("Starting {} for its start check…", file.display());
        start_check(&file, &build)?;
        println!("Packaged {} {} ({})", build.os, build.version, build.commit);
    } else {
        println!(
            "Packaged {} {} ({}) (no start check: this machine ({:?}) can't run a linux-x64 AppImage)",
            build.os, build.version, build.commit, host_target
        );
    }
    Ok(())
}

/// The AppImage's own start check (section 3.15): same scratch-home approach as `bundle`'s, but
/// its own process group — an AppImage's runtime runs the agent as its child, which would
/// otherwise keep running once the runtime itself is stopped — and `APPIMAGE_EXTRACT_AND_RUN=1`
/// when `/dev/fuse` isn't there to mount it (most CI containers).
fn start_check(file: &Path, build: &release::Build) -> Result<(), String> {
    let scratch = std::env::temp_dir().join(format!(
        "retaskable-package-check-{}-{}",
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

    let mut cmd = Command::new(file);
    cmd.env("HOME", &scratch)
        .env("XDG_DATA_HOME", &scratch)
        .env("XDG_STATE_HOME", &scratch)
        .env("RETASKABLE_PORT", port.to_string())
        .env("RETASKABLE_CREDENTIALS_FILE", scratch.join("credentials.json"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if !Path::new("/dev/fuse").exists() {
        cmd.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return cleanup(Err(format!("running {}: {e}", file.display()))),
    };

    let health = wait_for_health(port, &build.version);
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", child.id())])
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(e) => return cleanup(Err(format!("waiting for {}: {e}", file.display()))),
    };
    if let Err(e) = health {
        return cleanup(Err(e));
    }
    if !output.stdout.is_empty() || !output.stderr.is_empty() {
        return cleanup(Err(format!(
            "{} wrote to its terminal during the start check (a release build must only log to its file):\nstdout: {}\nstderr: {}",
            file.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let scratch_env = {
        let scratch = scratch.clone();
        move |k: &str| {
            matches!(k, "HOME" | "XDG_DATA_HOME" | "XDG_STATE_HOME").then(|| scratch.to_string_lossy().to_string())
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

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
