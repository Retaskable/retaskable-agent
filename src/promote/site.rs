//! Deploy from the website's own checks (`siteChecks` in `agent/src/promote.ts`, remote-promote.md section 3.2): before
//! the same push, staging must say it serves the commit and the person who pressed it must be an admin of the repo.
//! The first failing one refuses, and anything that can't be read refuses (fail closed).
use std::time::Duration;

use super::{Promoter, Stop, environments_of, host, short};

const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// `<url><versionPath>`: the address with its trailing slashes dropped and the path (default `/version.json`) after it.
pub(super) fn version_url(url: &str, version_path: Option<&str>) -> String {
    let path = version_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("/version.json");
    let sep = if path.starts_with('/') { "" } else { "/" };
    format!("{}{sep}{path}", url.trim_end_matches('/'))
}

/// Asks a deploy which commit it serves, at its version address: `{ "sha": "<commit>" }`, seven to forty hex digits.
/// `None` when it doesn't say, answers badly, or doesn't answer in 15 seconds.
pub(super) async fn served_sha(url: &str) -> Option<String> {
    let res = reqwest::Client::new()
        .get(url)
        .header("cache-control", "no-cache")
        .timeout(READ_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let body: serde_json::Value = res.json().await.ok()?;
    let sha = body.get("sha")?.as_str()?;
    let hex = (7..=40).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit());
    hex.then(|| sha.to_string())
}

/// `/^[a-z\d][a-z\d-]{0,38}$/i`: a GitHub login, the only thing that goes into the permission address.
pub(super) fn is_login(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && s.len() <= 39
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

impl Promoter {
    /// The checks alone, the first failing one refusing: staging has an address and serves a commit that starts with
    /// `sha`, `issued_by` looks like a login, and GitHub says it is an admin of the repo.
    pub async fn site_checks(&self, sha: &str, issued_by: &str) -> Result<(), Stop> {
        let env = environments_of(&self.deploy).ok_or_else(|| Stop::refused(super::run::NO_ENVIRONMENTS))?;
        let Some(url) = env.staging.url else {
            return Err(Stop::refused(
                "Staging has no address to check: deploy it from the machine.",
            ));
        };
        let path = self.deploy.staging.as_ref().and_then(|s| s.version_path.as_deref());
        let url = version_url(&url, path);
        let Some(serving) = served_sha(&url).await else {
            return Err(Stop::refused(format!(
                "Staging doesn't say which commit it serves at {url}: deploy it from the machine."
            )));
        };
        if !serving.to_lowercase().starts_with(&sha.to_lowercase()) {
            return Err(Stop::refused(format!(
                "Staging serves {}, not {}: check again.",
                short(&serving),
                short(sha)
            )));
        }
        if !is_login(issued_by) {
            return Err(Stop::refused("Only the repo's admins can deploy from the website."));
        }
        let permission = host::permission_of(&self.gh, &self.repo, &self.github_repo, issued_by)
            .await
            .map_err(|e| {
                Stop::refused(format!(
                    "Couldn't ask GitHub whether @{issued_by} is an admin of {}: {e}",
                    self.github_repo
                ))
            })?;
        if permission != "admin" {
            let has = if permission.is_empty() || permission == "none" {
                "no access"
            } else {
                &permission
            };
            return Err(Stop::refused(format!(
                "Only the repo's admins can deploy from the website (@{issued_by} has {has})."
            )));
        }
        Ok(())
    }

    /// Deploy from the website: [`Self::site_checks`], then at once [`Self::promote`] logged `via: "site"` under
    /// `issued_by`.
    pub async fn from_site(&self, sha: &str, issued_by: &str) -> Result<String, Stop> {
        self.site_checks(sha, issued_by).await?;
        self.promote(sha, super::Via::Site(issued_by.to_string())).await
    }
}
