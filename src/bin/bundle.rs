//! `cargo run --bin bundle -- [--target win-x64|mac-arm64|mac-x64|linux-x64] [--environment staging|production]
//! [--out <folder>] [--version <n.n.n>]` (installers/03-the-bundle-agent-bundle.md sections 3.9 and 3.10), by
//! default this machine's own target and Staging, into `dist/agent/<environment>-<target>/`: builds
//! `retaskable`'s release executable, writes `build.json` beside it, and, for the machine's own target,
//! checks that `--version` prints what was written and runs the start check (section 3.10): starts it as an
//! installed agent, with a scratch home so it touches neither this machine's own data folder nor its
//! credential store.
//!
//! `--target` can cross-build: a machine's own target, always, and — macOS only — the other Darwin
//! arch, since Apple's own `clang` targets both without an extra toolchain. Any other target needs a
//! machine of its own OS: win-x64 and linux-x64 each need a toolchain this port doesn't wire up for
//! cross-OS builds (`cross`/Docker, or a cross linker), so the three OSes' CI runners build win-x64,
//! linux-x64 and mac-arm64 natively and the macOS runner cross-builds mac-x64 beside its own, covering
//! all four. A cross-built target gets no `--version` or start check: it can't be run on this machine.
//! Nor does this check the Windows executable's PE subsystem (section 3.2/3.9): it isn't set to hide
//! the console yet, which needs the tray this port doesn't have.
use std::path::{Path, PathBuf};
use std::process::Command;

use retaskable_agent::release;
use retaskable_agent::release_check::{self, arg, free_port, wait_for_health};

fn main() {
    release_check::run_main(run);
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let environment = release_check::environment_arg(&args)?;
    let host_target = release::target_slug(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        format!(
            "this machine ({}-{}) isn't one of the bundle's targets yet",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let target = match arg(&args, "--target") {
        Some(t) if release::target_triple(&t).is_some() => t,
        Some(other) => {
            return Err(format!(
                "--target must be win-x64, mac-arm64, mac-x64 or linux-x64, not {other}"
            ));
        }
        None => host_target.to_string(),
    };
    if !buildable_here(host_target, &target) {
        return Err(format!(
            "{target} needs a machine running its own OS to build (this one's own target is {host_target}, and \
             only macOS can also cross-build its other arch without a cross toolchain this port doesn't wire up yet)"
        ));
    }
    let triple = release::target_triple(&target).expect("validated above");
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

    let native = target == host_target;
    println!("Building {target} ({}) into {}…", environment.name(), out.display());
    if !native {
        let status = Command::new("rustup")
            .args(["target", "add", triple])
            .status()
            .map_err(|e| format!("running rustup target add {triple}: {e}"))?;
        if !status.success() {
            return Err(format!("rustup target add {triple} failed"));
        }
    }
    let mut build_cmd = Command::new("cargo");
    build_cmd.args(["build", "--release", "--bin", "retaskable"]);
    if !native {
        build_cmd.args(["--target", triple]);
    }
    let status = build_cmd
        .current_dir(&manifest_dir)
        .status()
        .map_err(|e| format!("running cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build --release failed".to_string());
    }

    std::fs::create_dir_all(&out).map_err(|e| format!("creating {}: {e}", out.display()))?;
    let exe_name = if target.starts_with("win") {
        "retaskable.exe"
    } else {
        "retaskable"
    };
    let build_dir = if native {
        target_dir.join("release")
    } else {
        target_dir.join(triple).join("release")
    };
    let built = build_dir.join(exe_name);
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

    if !native {
        println!(
            "Built {target} {version} ({commit}) at {} (cross-built: no --version or start check, since this machine can't run it)",
            out.display()
        );
        return Ok(());
    }

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

/// Whether `requested` can be built on a machine whose own target is `host`: itself, or — macOS
/// only — the other Darwin arch, which needs no extra toolchain (Apple's own `clang` targets
/// both). Any other combination needs a cross toolchain this port doesn't wire up yet (section 3.9).
fn buildable_here(host: &str, requested: &str) -> bool {
    requested == host || (host.starts_with("mac-") && requested.starts_with("mac-"))
}

/// Section 3.10's start check: starts `placed` as an installed agent is started — a scratch home, so its
/// folders are new, and its secrets go to a file there rather than this machine's own credential store
/// (section 3.10's 2026-10-18 incident, when a scratch run spent and lost this machine's own refresh token
/// by sharing its credential store instead). Fails unless, within 60 seconds, `GET /api/health` answers
/// `build`'s version, the log has the `Links:` line saying not linked, and nothing reached the terminal.
fn start_check(placed: &Path, build: &release::Build) -> Result<(), String> {
    let scratch = release_check::ScratchDir::new("retaskable-bundle-check")?;
    let port = free_port().map_err(|e| format!("finding a free port for the start check: {e}"))?;

    let mut child = Command::new(placed)
        .env("HOME", scratch.path())
        .env("USERPROFILE", scratch.path())
        .env("LOCALAPPDATA", scratch.path())
        .env("XDG_DATA_HOME", scratch.path())
        .env("XDG_STATE_HOME", scratch.path())
        .env("RETASKABLE_PORT", port.to_string())
        .env("RETASKABLE_CREDENTIALS_FILE", scratch.path().join("credentials.json"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {}: {e}", placed.display()))?;

    let health = wait_for_health(port, &build.version);
    let _ = child.kill();
    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting for {}: {e}", placed.display()))?;
    health?;
    release_check::check_silent_terminal(placed, &output)?;
    release_check::check_not_linked_log(build, scratch.path())
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
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_can_always_build_its_own_target() {
        assert!(buildable_here("win-x64", "win-x64"));
        assert!(buildable_here("mac-arm64", "mac-arm64"));
        assert!(buildable_here("mac-x64", "mac-x64"));
        assert!(buildable_here("linux-x64", "linux-x64"));
    }

    #[test]
    fn macos_can_cross_build_its_other_arch() {
        assert!(buildable_here("mac-arm64", "mac-x64"));
        assert!(buildable_here("mac-x64", "mac-arm64"));
    }

    #[test]
    fn nothing_else_crosses_without_a_toolchain() {
        assert!(!buildable_here("win-x64", "mac-arm64"));
        assert!(!buildable_here("win-x64", "linux-x64"));
        assert!(!buildable_here("linux-x64", "win-x64"));
        assert!(!buildable_here("linux-x64", "mac-x64"));
        assert!(!buildable_here("mac-arm64", "linux-x64"));
        assert!(!buildable_here("mac-arm64", "win-x64"));
    }

    #[test]
    fn is_dotted_version_requires_exactly_three_numeric_parts() {
        assert!(is_dotted_version("1.4.2"));
        assert!(is_dotted_version("0.1.812"));
        assert!(!is_dotted_version("5"));
        assert!(!is_dotted_version("1.2"));
        assert!(!is_dotted_version("1.2.3.4"));
        assert!(!is_dotted_version(""));
        assert!(!is_dotted_version("1.2.a"));
        assert!(!is_dotted_version("1..2"));
        assert!(!is_dotted_version("1.2."));
    }
}
