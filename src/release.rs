//! A release build's own identity and where its files go, following `agent/src/release.ts` and
//! `agent/src/paths.ts` in Retaskable.com (installers/03-the-bundle-agent-bundle.md sections 3.3
//! and 3.7). A checkout has none of this: it keeps `.retaskable/` where it's started
//! (`config::data_dir`), as before.
#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tracing_subscriber::fmt::writer::MakeWriter;

/// `build.json`'s `environment`: which cloud a release build is fixed to (tray.md decision 7).
/// Any other value fails to parse, so a bad one is `BUILD_PROBLEM`, not a silent guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Production,
    Staging,
}

impl Environment {
    /// `build.json`'s own spelling: `"production"` or `"staging"` (also `link::environment_url`'s name).
    pub fn name(self) -> &'static str {
        match self {
            Environment::Production => "production",
            Environment::Staging => "staging",
        }
    }
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

/// `releaseName` (release.ts): `Retaskable` or `Retaskable Staging`, used wherever a release
/// build names itself to a person (a desktop entry's `Name=`, a login item's).
pub fn release_name(environment: Environment) -> &'static str {
    names(environment).0
}

/// installers/03-the-bundle-agent-bundle.md section 3.13: the port a release build's health
/// check falls back to once neither `RETASKABLE_PORT` nor the config's own `port` says a valid
/// one — a Staging release build's own is 4701, so it never collides with a Production or
/// checkout agent on the same machine; every other build (`None` is a checkout) keeps
/// `config::DEFAULT_PORT` (4700).
pub fn default_port(build: Option<&Build>) -> u16 {
    match build.map(|b| b.environment) {
        Some(Environment::Staging) => 4701,
        _ => crate::config::DEFAULT_PORT,
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

/// The `rustc`/`cargo --target` triple for one of `target_slug`'s four slugs, or `None` for
/// anything else (section 3.9: `bundle` cross-builds only these).
pub fn target_triple(slug: &str) -> Option<&'static str> {
    match slug {
        "win-x64" => Some("x86_64-pc-windows-msvc"),
        "mac-arm64" => Some("aarch64-apple-darwin"),
        "mac-x64" => Some("x86_64-apple-darwin"),
        "linux-x64" => Some("x86_64-unknown-linux-gnu"),
        _ => None,
    }
}

/// The line `releaseStart.ts` logs first, naming the build: "Retaskable Staging 1.4.2 (abc1234)
/// on win-x64" (section 3.7).
pub fn build_line(build: &Build) -> String {
    let (title, _) = names(build.environment);
    format!("{title} {} ({}) on {}", build.version, build.commit, build.os)
}

/// Section 3.7: a release build's log file, rotated at 10 MB with 3 old ones kept (`.1` to
/// `.3`), since there's no terminal to read `console.log` from. A checkout logs to its terminal
/// instead, as before.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
const KEPT_LOGS: u32 = 3;

struct RotatingLogInner {
    path: PathBuf,
    file: File,
    len: u64,
    max_bytes: u64,
}

/// A clonable handle `tracing_subscriber` can hand a fresh `Write` from for each event.
#[derive(Clone)]
pub struct RotatingLog(Arc<Mutex<RotatingLogInner>>);

impl RotatingLog {
    /// Opens (or creates and appends to) `dir`'s `retaskable.log`.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        Self::open_with_max(dir, MAX_LOG_BYTES)
    }

    /// `open`, with the rotation size picked by the caller (a test, so it doesn't have to write
    /// real megabytes to see a rotation happen).
    fn open_with_max(dir: &Path, max_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("retaskable.log");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self(Arc::new(Mutex::new(RotatingLogInner {
            path,
            file,
            len,
            max_bytes,
        }))))
    }
}

impl<'a> MakeWriter<'a> for RotatingLog {
    type Writer = RotatingLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingLogWriter(self.0.clone())
    }
}

pub struct RotatingLogWriter(Arc<Mutex<RotatingLogInner>>);

impl Write for RotatingLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut inner = self.0.lock().unwrap();
        if inner.len >= inner.max_bytes {
            rotate(&mut inner)?;
        }
        let n = inner.file.write(buf)?;
        inner.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().file.flush()
    }
}

/// Shifts `.1` to `.2`, `.2` to `.3` (dropping the old `.3`), moves the current file to `.1`, and
/// starts a fresh one.
fn rotate(inner: &mut RotatingLogInner) -> std::io::Result<()> {
    for n in (1..KEPT_LOGS).rev() {
        let from = numbered(&inner.path, n);
        if from.exists() {
            std::fs::rename(&from, numbered(&inner.path, n + 1))?;
        }
    }
    std::fs::rename(&inner.path, numbered(&inner.path, 1))?;
    inner.file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&inner.path)?;
    inner.len = 0;
    Ok(())
}

fn numbered(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}
