//! A release build's own updates (installers/03-the-bundle-agent-bundle.md section 3.11), following
//! `agent/src/releaseUpdate.ts` and `shared/agentRelease.ts` in Retaskable.com. It has no checkout to
//! move forward, so at start and every hour it asks its environment's release feed
//! (`GET /api/agent/release?os=&arch=`) for the newest build for its target. A newer one is downloaded
//! into the data folder's `updates/<version>/`, and kept only once its SHA-256 is the feed's: a wrong
//! one is deleted and refused. Readying a downloaded build for install (a signature check unlike the
//! running app's refused too) and actually swapping it in on a restart are section 3.12
//! (`releaseInstall.ts`, ported as [`crate::release_install`]) — [`Installer`] is the seam between
//! the two.
#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use regex::Regex;
use serde_json::Value;

use crate::release::Build;

/// `shared/agentRelease.ts`'s `RELEASE_TARGETS`: the bundle's four targets, as `<os>-<arch>`.
const RELEASE_TARGETS: [&str; 4] = ["win-x64", "mac-arm64", "mac-x64", "linux-x64"];

/// `<os>-<arch>` when it's one of the bundle's four targets, else `None` (`releaseTarget`).
pub fn release_target(os: &str, arch: &str) -> Option<&'static str> {
    RELEASE_TARGETS.iter().find(|t| **t == format!("{os}-{arch}")).copied()
}

/// A release feed entry (`GET /api/agent/release?os=&arch=`): a version of dotted numbers, an https
/// URL and its SHA-256, 64 lowercase hex digits (`AgentRelease`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRelease {
    pub version: String,
    pub url: String,
    pub sha256: String,
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+(\.\d+){0,3}$").unwrap());
static SHA256_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{64}$").unwrap());

/// `value`, or `None` when it isn't a release feed entry (`parseAgentRelease`).
pub fn parse_agent_release(value: &Value) -> Option<AgentRelease> {
    let obj = value.as_object()?;
    let version = obj.get("version")?.as_str()?;
    if !VERSION_RE.is_match(version) {
        return None;
    }
    let sha256 = obj.get("sha256")?.as_str()?;
    if !SHA256_RE.is_match(sha256) {
        return None;
    }
    let url = obj.get("url")?.as_str()?;
    if reqwest::Url::parse(url).map(|u| u.scheme() != "https").unwrap_or(true) {
        return None;
    }
    Some(AgentRelease {
        version: version.to_string(),
        url: url.to_string(),
        sha256: sha256.to_string(),
    })
}

/// Whether `a` is a later version than `b`, number by number ("1.10.0" is later than "1.9.3").
pub fn is_newer_version(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<i64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let d = pa.get(i).copied().unwrap_or(0) - pb.get(i).copied().unwrap_or(0);
        if d != 0 {
            return d > 0;
        }
    }
    false
}

/// `<cloud>/api/agent/release?os=<os>&arch=<arch>`, for a build whose `os` is `<os>-<arch>` (one of the
/// bundle's four targets); `None` for any other (`releaseFeedUrl`).
pub fn release_feed_url(cloud: &str, build_os: &str) -> Option<String> {
    let (os, arch) = build_os.split_once('-')?;
    release_target(os, arch)?;
    let base = reqwest::Url::parse(cloud).ok()?;
    let mut url = base.join("/api/agent/release").ok()?;
    url.query_pairs_mut().append_pair("os", os).append_pair("arch", arch);
    Some(url.to_string())
}

/// The download's file name: the URL's last path segment, percent-decoded, kept to letters, digits,
/// dots, dashes and underscores, with any leading dots stripped (`updateFileName`).
pub fn update_file_name(url: &str) -> String {
    let last = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)))
        .unwrap_or_default();
    let decoded = percent_decode(&last);
    let cleaned: String = decoded
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stripped = cleaned.trim_start_matches('.');
    if stripped.is_empty() {
        "update".to_string()
    } else {
        stripped.to_string()
    }
}

/// A minimal `decodeURIComponent`: `%XX` to its byte, everything else left alone.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A newer build, downloaded and checked: `file` is in `updates/<version>/`, and `install` what the
/// installer readied from it (the file itself, or the `.app` unpacked from it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyUpdate {
    pub version: String,
    pub file: PathBuf,
    pub install: PathBuf,
}

/// Left in `updates/<version>/` just before that version is installed: a copy that starts again still
/// older than it doesn't try it again (and restart for it on every start) until a newer one is out.
pub const TRIED_FILE: &str = "tried";

/// Checked at start and every hour after (`UPDATE_CHECK_EVERY_MS`).
pub const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// How a downloaded build replaces this copy (`releaseInstall.ts`'s `Installer`, section 3.12). No
/// implementation exists in this port yet; a download is only kept until one does.
pub trait Installer: Send + Sync {
    /// Why this copy can't replace itself (e.g. it isn't running as an AppImage), or `None` if it can.
    fn cannot(&self) -> Option<String> {
        None
    }
    /// Readies `file` for install (e.g. unpacking an `.app`); errors on a signature it refuses.
    fn prepare(&self, file: &Path) -> Result<PathBuf, String> {
        Ok(file.to_path_buf())
    }
    /// Installs `prepared` in this copy's place, or errors with why it couldn't.
    fn install(&self, prepared: &Path) -> Result<bool, String>;
}

/// A plain answer to one `GET`: `fetch`'s own `{ status, body }`, so a test can hand back a canned one
/// without a real socket (`deps.fetch` in the TypeScript agent).
pub struct FetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// `GET`'s the feed and the download; swapped out by a test the same way `deps.fetch` is in the
/// TypeScript agent, so neither needs a real socket (the feed's own URL is a release build's fixed
/// cloud, and the download's is whatever https address the feed names — not addresses this crate's
/// own test fixtures can stand up a real TLS listener for).
pub type FetchFn = Box<dyn Fn(String) -> BoxFuture<anyhow::Result<FetchResponse>> + Send + Sync>;

/// Called with a newly downloaded [`ReadyUpdate`], so `main.rs` can ask for a restart.
pub type OnReadyFn = Box<dyn Fn(&ReadyUpdate) + Send + Sync>;

/// Replaces `tracing::info!` for a test that wants the messages back.
pub type LogFn = Box<dyn Fn(&str) + Send + Sync>;

/// What a [`ReleaseUpdater`] needs: its own build, the environment it's fixed to, where its downloads
/// go, and the seams a test (or, once section 3.12 is ported, the real installer and restart) replaces.
pub struct ReleaseUpdateDeps {
    pub build: Build,
    /// The environment's URL: a release build's is fixed by `build.json` (`link::cloud_url`).
    pub cloud_url: String,
    /// The data folder, whose `updates/` holds the download.
    pub data_dir: PathBuf,
    pub http: reqwest::Client,
    /// How it replaces itself; with none, a download is only kept.
    pub installer: Option<Arc<dyn Installer>>,
    /// A newer build is ready to install: `main.rs` would ask for a restart, once one exists to ask.
    pub on_ready: Option<OnReadyFn>,
    /// Defaults to `tracing::info!`; a test replaces it to read the messages back.
    pub log: Option<LogFn>,
    /// Defaults to `http`; a test replaces it to answer without a real socket.
    pub fetch: Option<FetchFn>,
}

/// Asks the environment's release feed for a newer build and downloads it (`ReleaseUpdater`).
pub struct ReleaseUpdater {
    deps: ReleaseUpdateDeps,
    /// The newest build downloaded and checked, once there is one.
    ready: Mutex<Option<ReadyUpdate>>,
    /// A version already said to be skipped, so it's said once.
    skipped: Mutex<Option<String>>,
}

impl ReleaseUpdater {
    pub fn new(deps: ReleaseUpdateDeps) -> Self {
        Self {
            deps,
            ready: Mutex::new(None),
            skipped: Mutex::new(None),
        }
    }

    /// The newest build downloaded and checked, once there is one.
    pub fn ready(&self) -> Option<ReadyUpdate> {
        self.ready.lock().unwrap().clone()
    }

    fn updates_dir(&self) -> PathBuf {
        self.deps.data_dir.join("updates")
    }

    fn log(&self, msg: &str) {
        match &self.deps.log {
            Some(f) => f(&format!("Update: {msg}")),
            None => tracing::info!("Update: {msg}"),
        }
    }

    /// `GET url`, through the injected fake when a test supplied one, else the real `http` client.
    async fn fetch(&self, url: &str) -> anyhow::Result<FetchResponse> {
        match &self.deps.fetch {
            Some(f) => f(url.to_string()).await,
            None => {
                let res = self.deps.http.get(url).send().await?;
                let status = res.status().as_u16();
                let body = res.bytes().await?.to_vec();
                Ok(FetchResponse { status, body })
            }
        }
    }

    /// Asks the feed once, downloading a newer build. Never panics or propagates: what went wrong is
    /// logged, and the caller (the next hour's [`watch`]) tries again.
    pub async fn check(&self) -> Option<ReadyUpdate> {
        match self.check_once().await {
            Ok(ready) => ready,
            Err(e) => {
                self.log(&format!("couldn't check for a newer version ({e})."));
                self.ready()
            }
        }
    }

    async fn check_once(&self) -> anyhow::Result<Option<ReadyUpdate>> {
        let build = &self.deps.build;
        let feed = release_feed_url(&self.deps.cloud_url, &build.os)
            .ok_or_else(|| anyhow::anyhow!("no release feed for {}", build.os))?;
        let res = self.fetch(&feed).await?;
        if res.status == reqwest::StatusCode::NOT_FOUND.as_u16() {
            return Ok(self.ready());
        }
        if !(200..300).contains(&res.status) {
            anyhow::bail!("the feed answered {}", res.status);
        }
        let body: Value = serde_json::from_slice(&res.body)?;
        let release = parse_agent_release(&body).ok_or_else(|| anyhow::anyhow!("the feed's answer isn't a release"))?;
        if !is_newer_version(&release.version, &build.version) {
            return Ok(self.ready());
        }
        if self.ready().map(|r| r.version) == Some(release.version.clone()) {
            return Ok(self.ready());
        }
        let cannot = self.deps.installer.as_ref().and_then(|i| i.cannot());
        let tried = self.updates_dir().join(&release.version).join(TRIED_FILE).exists();
        if cannot.is_some() || tried {
            let already_said = {
                let mut skipped = self.skipped.lock().unwrap();
                let said = skipped.as_deref() == Some(release.version.as_str());
                *skipped = Some(release.version.clone());
                said
            };
            if !already_said {
                self.log(&match &cannot {
                    Some(c) => format!("{} is out, but this copy can't replace itself ({c}).", release.version),
                    None => format!(
                        "{} was installed once and this is still {}, so it isn't tried again until a newer one is out.",
                        release.version, build.version
                    ),
                });
            }
            return Ok(self.ready());
        }
        let ready = self.download(&release).await?;
        self.log(&format!(
            "{} downloaded and checked, {}.",
            ready.version,
            ready.file.display()
        ));
        // Stored before `on_ready` runs: it asks `main.rs` for a restart, whose exit reads
        // `ready()` back through `install_now` — on another thread, but not so late that this
        // copy's own write can be assumed to have landed first.
        *self.ready.lock().unwrap() = Some(ready.clone());
        if let Some(on_ready) = &self.deps.on_ready {
            on_ready(&ready);
        }
        Ok(Some(ready))
    }

    /// Just before a restart's exit: puts the ready build in place and starts it. `false` when there's
    /// none ready or it couldn't (logged), so the agent starts its own copy again instead.
    pub fn install_now(&self) -> bool {
        let (ready, installer) = match (self.ready(), &self.deps.installer) {
            (Some(ready), Some(installer)) => (ready, installer),
            _ => return false,
        };
        let attempt = || -> anyhow::Result<bool> {
            let tried_path = self.updates_dir().join(&ready.version).join(TRIED_FILE);
            std::fs::write(&tried_path, now_iso8601())?;
            self.log(&format!("installing {}.", ready.version));
            installer.install(&ready.install).map_err(|e| anyhow::anyhow!(e))
        };
        match attempt() {
            Ok(installed) => installed,
            Err(e) => {
                self.log(&format!(
                    "couldn't install {} ({e}), so this copy starts again.",
                    ready.version
                ));
                false
            }
        }
    }

    /// Into `updates/<version>/`, as `.part` until its hash is the feed's, then readied by the
    /// installer; other versions' downloads go. One already there whose hash is right isn't downloaded
    /// again. On any error the version's folder is removed, so a refused or half-written download
    /// isn't mistaken for a ready one later.
    async fn download(&self, release: &AgentRelease) -> anyhow::Result<ReadyUpdate> {
        let updates_dir = self.updates_dir();
        let dir = updates_dir.join(&release.version);
        let file = dir.join(update_file_name(&release.url));
        if let Ok(entries) = std::fs::read_dir(&updates_dir) {
            for entry in entries.flatten() {
                if entry.file_name() != release.version.as_str() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        match self.download_inner(&dir, &file, release).await {
            Ok(install) => Ok(ReadyUpdate {
                version: release.version.clone(),
                file,
                install,
            }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }

    async fn download_inner(&self, dir: &Path, file: &Path, release: &AgentRelease) -> anyhow::Result<PathBuf> {
        let already_right = file.exists() && sha256_file(file)? == release.sha256;
        if !already_right {
            std::fs::create_dir_all(dir)?;
            let res = self.fetch(&release.url).await?;
            if !(200..300).contains(&res.status) {
                anyhow::bail!("the download answered {}", res.status);
            }
            let bytes = res.body;
            if sha256_hex(&bytes) != release.sha256 {
                anyhow::bail!(
                    "the download of {} isn't the build the feed names (its SHA-256 differs), so it was refused",
                    release.version
                );
            }
            let part = part_path(file);
            std::fs::write(&part, &bytes)?;
            let _ = std::fs::remove_file(file);
            std::fs::rename(&part, file)?;
        }
        match &self.deps.installer {
            Some(installer) => installer.prepare(file).map_err(|e| anyhow::anyhow!(e)),
            None => Ok(file.to_path_buf()),
        }
    }
}

fn part_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    Ok(sha256_hex(&std::fs::read(path)?))
}

fn now_iso8601() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Checks now and every hour after, until the process exits (`ReleaseUpdater.start`'s `setInterval` in
/// the TypeScript agent); nothing here needs a `stop`, since nothing restarts this loop without
/// restarting the process (the same pattern as `phone::watch`/`local_model::watch`).
pub async fn watch(updater: Arc<ReleaseUpdater>) -> ! {
    loop {
        updater.check().await;
        tokio::time::sleep(UPDATE_CHECK_EVERY).await;
    }
}
