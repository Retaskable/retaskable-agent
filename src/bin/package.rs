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
use retaskable_agent::release;
use retaskable_agent::release_check::{self, arg, free_port, wait_for_health};

fn main() {
    release_check::run_main(run);
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let environment = release_check::environment_arg(&args)?;
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
    let scratch = release_check::ScratchDir::new("retaskable-package-check")?;
    let port = free_port().map_err(|e| format!("finding a free port for the start check: {e}"))?;

    let mut cmd = Command::new(file);
    cmd.env("HOME", scratch.path())
        .env("XDG_DATA_HOME", scratch.path())
        .env("XDG_STATE_HOME", scratch.path())
        .env("RETASKABLE_PORT", port.to_string())
        .env("RETASKABLE_CREDENTIALS_FILE", scratch.path().join("credentials.json"))
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
    #[allow(unused_mut)]
    let mut child = cmd.spawn().map_err(|e| format!("running {}: {e}", file.display()))?;

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
    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting for {}: {e}", file.display()))?;
    health?;
    release_check::check_silent_terminal(file, &output)?;
    release_check::check_not_linked_log(build, scratch.path())
}
