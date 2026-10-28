//! `cargo run --bin bundle -- [--environment staging|production] [--out <folder>] [--version <n.n.n>]`
//! (installers/03-the-bundle-agent-bundle.md section 3.9), by default this machine's target and
//! Staging, into `dist/agent/<environment>-<target>/`: builds `retaskable`'s release executable,
//! writes `build.json` beside it, and checks that `--version` prints what was written.
//!
//! Only the machine's own target is built here (`release::target_slug`); the TypeScript agent's
//! `npm run bundle` can also cross-build the other three, which this doesn't do yet.
use std::path::PathBuf;
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

    println!("Built {target} {version} ({commit}) at {}", out.display());
    Ok(())
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
