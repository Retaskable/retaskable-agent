//! A release build's own identity and where its files go, following `agent/src/release.ts` and
//! `agent/src/paths.ts` in Retaskable.com (installers/03-the-bundle-agent-bundle.md sections 3.3
//! and 3.7). A checkout has none of this: it keeps `.retaskable/` where it's started
//! (`config::data_dir`), as before.
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `build.json`'s `environment`: which cloud a release build is fixed to (tray.md decision 7).
/// Any other value fails to parse, so a bad one is `BUILD_PROBLEM`, not a silent guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Production,
    Staging,
}

/// `build.json`, beside the executable: `{ os, environment, version, commit }` (section 3.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Build {
    pub os: String,
    pub environment: Environment,
    pub version: String,
    pub commit: String,
}

/// `loadBuild` (release.ts): `build.json` beside `exe_path`, or `Ok(None)` in a checkout, which
/// has none. `Err` is `BUILD_PROBLEM` (section 3.8): the file is there but couldn't be read or
/// parsed, so the caller refuses to start rather than guess at a release build's identity.
pub fn load_build(exe_path: &Path) -> Result<Option<Build>, String> {
    let dir = exe_path.parent().unwrap_or(Path::new("."));
    let text = match std::fs::read_to_string(dir.join("build.json")) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(problem(exe_path, &e)),
    };
    serde_json::from_str(&text).map(Some).map_err(|e| problem(exe_path, &e))
}

fn problem(exe_path: &Path, err: &impl std::fmt::Display) -> String {
    format!(
        "its build.json, beside {}, couldn't be read ({err})",
        exe_path.display()
    )
}

/// Section 3.3: a release build's data folder (`machine.json`, each project's `state.json`,
/// `runs/`), named for its environment, in the OS's own place — never the `.retaskable` a
/// checkout uses.
pub fn data_dir(build: &Build, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let (title, slug) = names(build.environment);
    if cfg!(target_os = "windows") {
        local_app_data(&env).join(title)
    } else if cfg!(target_os = "macos") {
        home(&env).join("Library").join("Application Support").join(title)
    } else {
        xdg(&env, "XDG_DATA_HOME", ".local/share").join(slug)
    }
}

/// Section 3.3: a release build's log folder.
pub fn logs_dir(build: &Build, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let (title, slug) = names(build.environment);
    if cfg!(target_os = "windows") {
        local_app_data(&env).join(title).join("logs")
    } else if cfg!(target_os = "macos") {
        home(&env).join("Library").join("Logs").join(title)
    } else {
        xdg(&env, "XDG_STATE_HOME", ".local/state").join(slug).join("logs")
    }
}

/// `Retaskable`/`Retaskable Staging` (Windows, macOS); `retaskable`/`retaskable-staging` (Linux).
fn names(environment: Environment) -> (&'static str, &'static str) {
    match environment {
        Environment::Production => ("Retaskable", "retaskable"),
        Environment::Staging => ("Retaskable Staging", "retaskable-staging"),
    }
}

fn local_app_data(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    PathBuf::from(env("LOCALAPPDATA").unwrap_or_default())
}

fn home(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    PathBuf::from(env("HOME").unwrap_or_default())
}

fn xdg(env: &impl Fn(&str) -> Option<String>, var: &str, fallback: &str) -> PathBuf {
    env(var).map(PathBuf::from).unwrap_or_else(|| home(env).join(fallback))
}

/// `win-x64`/`mac-arm64`/`mac-x64`/`linux-x64` (section 3.1), or `None` for a machine the bundle
/// isn't built for yet (`linux-arm64`, section 3.1's "later").
pub fn target_slug(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("win-x64"),
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        ("linux", "x86_64") => Some("linux-x64"),
        _ => None,
    }
}
