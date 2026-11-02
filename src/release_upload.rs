//! `cargo run --bin upload-release -- --environment staging|production` (installers/03-the-bundle-agent-bundle.md
//! section 3.16), the port of `scripts/uploadRelease.ts`: publishes the AppImage `package` made to the
//! environment's `retaskable-agent-releases-<environment>` R2 bucket at `downloads/<version>/<file>`, which the
//! hub serves at `/api/agent/download/<version>/<file>`, then the target's feed entry `<os>-<arch>.json` naming
//! it, so a release build never hears of a package that isn't there yet.
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::package::package_name;
use crate::release::{self, Environment};

/// Every target the feed knows; the ones not in `PUBLISHED_TARGETS` are named and skipped.
const RELEASE_TARGETS: [&str; 4] = ["win-x64", "mac-arm64", "mac-x64", "linux-x64"];
/// macOS and Windows ship the desktop app instead, on its own feed.
const PUBLISHED_TARGETS: [&str; 1] = ["linux-x64"];

/// One target's package and the feed entry that names it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseUpload {
    pub target: String,
    pub file: PathBuf,
    pub key: String,
    pub content_type: &'static str,
    pub entry: serde_json::Value,
}

fn origin(environment: Environment) -> &'static str {
    crate::link::environment_url(environment.name()).expect("both release environments have a URL")
}

fn content_type(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|e| e.to_str()) {
        Some("zip") => "application/zip",
        Some("AppImage") => "application/vnd.appimage",
        Some("exe") => "application/vnd.microsoft.portable-executable",
        _ => "application/octet-stream",
    }
}

/// What to publish: each published target with a bundle in `bundles` (`<environment>-<target>/build.json`) and
/// its package in `packages`. The rest are skipped, with why.
pub fn release_uploads(
    environment: Environment,
    bundles: &Path,
    packages: &Path,
) -> Result<(Vec<ReleaseUpload>, Vec<String>), String> {
    let mut uploads = Vec::new();
    let mut skipped = Vec::new();
    for target in RELEASE_TARGETS {
        if !PUBLISHED_TARGETS.contains(&target) {
            skipped.push(format!("{target}: the desktop app's"));
            continue;
        }
        let dir = bundles.join(format!("{}-{target}", environment.name()));
        // `load_build` looks beside an executable, so name one.
        let build = match release::load_build(&dir.join("retaskable"))? {
            Some(b) => b,
            None => {
                skipped.push(format!("{target}: no bundle"));
                continue;
            }
        };
        let Ok(name) = package_name(&build) else {
            skipped.push(format!("{target}: no package for it yet"));
            continue;
        };
        let file = packages.join(&name);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(_) => {
                skipped.push(format!("{target}: {name} isn't packaged"));
                continue;
            }
        };
        let sha256: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        let entry = json!({
            "version": build.version,
            "url": format!("{}/api/agent/download/{}/{name}", origin(environment), build.version),
            "sha256": sha256,
        });
        if crate::release_update::parse_agent_release(&entry).is_none() {
            return Err(format!(
                "{name}'s feed entry wouldn't be one (version {})",
                build.version
            ));
        }
        uploads.push(ReleaseUpload {
            target: target.to_string(),
            key: format!("downloads/{}/{name}", build.version),
            content_type: content_type(&name),
            file,
            entry,
        });
    }
    Ok((uploads, skipped))
}

/// Puts one object into the bucket.
pub trait Put {
    fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), String>;
}

/// Each package, then its feed entry: a failed package put returns before the entry is written.
pub fn publish(uploads: &[ReleaseUpload], put: &dyn Put) -> Result<(), String> {
    for u in uploads {
        let body = std::fs::read(&u.file).map_err(|e| format!("reading {}: {e}", u.file.display()))?;
        put.put(&u.key, body, u.content_type)?;
        let entry = format!("{}\n", u.entry);
        put.put(&format!("{}.json", u.target), entry.into_bytes(), "application/json")?;
    }
    Ok(())
}

/// Cloudflare's R2 object API (the one Wrangler's `r2 object put --remote` calls), with the Woodpecker secrets.
pub struct R2 {
    base: String,
    token: String,
    bucket: String,
    client: reqwest::blocking::Client,
}

impl R2 {
    /// `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` from the environment only; either missing fails.
    pub fn from_env(environment: Environment, get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    format!("{name} isn't set: upload-release takes the Cloudflare credentials from CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID (CI's secrets)")
                })
        };
        let token = var("CLOUDFLARE_API_TOKEN")?;
        let account = var("CLOUDFLARE_ACCOUNT_ID")?;
        Ok(R2 {
            base: format!("https://api.cloudflare.com/client/v4/accounts/{account}/r2/buckets"),
            token,
            bucket: format!("retaskable-agent-releases-{}", environment.name()),
            client: reqwest::blocking::Client::new(),
        })
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }
}

impl Put for R2 {
    fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), String> {
        println!("Uploading {}/{key}", self.bucket);
        let url = format!("{}/{}/objects/{key}", self.base, self.bucket);
        let res = self
            .client
            .put(&url)
            .bearer_auth(&self.token)
            .header("Content-Type", content_type)
            .body(body)
            .send()
            .map_err(|e| format!("couldn't put {}/{key}: {e}", self.bucket))?;
        if !res.status().is_success() {
            return Err(format!("couldn't put {}/{key} (HTTP {})", self.bucket, res.status()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
