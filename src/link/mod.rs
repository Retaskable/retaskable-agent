//! Linking this agent to retaskable.com (Retaskable.com agent/src/link.ts, cloud.md section 4.2): the OAuth device
//! authorization grant (RFC 8628), like `gh auth login`. The refresh token goes in the credential store
//! (`credentials`), one per URL, and is swapped for short-lived access tokens (`DeviceTokens`).
mod credentials;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

pub use credentials::{FileStore, TokenStore, credential_file_note, credentials_file, keychain};

use crate::config::ConfigFile;

/// retaskable.com's three environments (environments.md section 2), in the order the tray lists them.
pub const ENVIRONMENTS: [(&str, &str); 3] = [
    ("production", "https://retaskable.com"),
    ("staging", "https://staging.retaskable.com"),
    ("local", "http://localhost:8787"),
];

const DEFAULT_CLOUD_URL: &str = ENVIRONMENTS[0].1;

/// The device-code grant's `grant_type` (shared/deviceGrant.ts).
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The URL of the environment named `name` (`production`, `staging` or `local`).
pub fn environment_url(name: &str) -> Option<&'static str> {
    ENVIRONMENTS.iter().find(|(n, _)| *n == name).map(|(_, url)| *url)
}

/// Production, Staging or Local for retaskable.com's three, else Custom.
pub fn environment_name(url: &str) -> &'static str {
    match url.trim_end_matches('/') {
        "https://retaskable.com" => "Production",
        "https://staging.retaskable.com" => "Staging",
        "http://localhost:8787" => "Local",
        _ => "Custom",
    }
}

/// The cloud this agent links to: `RETASKABLE_CLOUD_URL`, else `cloud.url`, else retaskable.com. No trailing slash.
/// Errors on an address that isn't http or https, before anything is sent.
pub fn cloud_url(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> Result<String> {
    let url = env("RETASKABLE_CLOUD_URL")
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .or_else(|| {
            cfg.machine
                .cloud
                .url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| DEFAULT_CLOUD_URL.to_string());
    let parsed = Url::parse(&url).with_context(|| format!("The cloud URL isn't an address: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("The cloud URL must be http or https: {url}");
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Every cloud this agent connects to (environments.md section 3.4): the three environments, then the agent's own
/// cloud URL when it's none of them, the Custom one. A bad `cloud.url` is logged, and the three still connect. Every
/// agent is a development build until installers exist (tray.md decision 7), so this is never just one.
pub fn cloud_urls(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut urls: Vec<String> = ENVIRONMENTS.iter().map(|(_, url)| url.to_string()).collect();
    match cloud_url(cfg, env) {
        Ok(own) if !urls.contains(&own) => urls.push(own),
        Ok(_) => {}
        Err(e) => tracing::warn!("cloud.url isn't a valid address: {e:#}"),
    }
    urls
}

/// What a link is to: retaskable.com's device endpoints are under `/api/device`.
#[derive(Debug, Clone, Copy)]
pub struct LinkTarget {
    pub name: &'static str,
    pub device_path: &'static str,
    /// Where the URL comes from, for a message when it can't be reached.
    pub setting: &'static str,
}

pub const CLOUD_TARGET: LinkTarget = LinkTarget {
    name: "cloud",
    device_path: "/api/device",
    setting: "cloud.url or RETASKABLE_CLOUD_URL",
};

/// The name this machine goes by on retaskable.com: `RETASKABLE_MACHINE` when set (a container's hostname is a random
/// id, docker.md section 3), else the hostname.
pub fn machine_name(env: impl Fn(&str) -> Option<String>) -> String {
    env("RETASKABLE_MACHINE")
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .or_else(|| Some(gethostname::gethostname().to_string_lossy().to_string()).filter(|h| !h.is_empty()))
        .unwrap_or_else(|| "unknown".to_string())
}

/// What the `/link` page shows about this machine. `version` is this crate's own in a checkout,
/// or a release build's `build.json` one.
pub fn machine_info(env: impl Fn(&str) -> Option<String>, version: &str) -> Value {
    let os = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    json!({
        "machine": truncate(&machine_name(env), 100),
        "os": os,
        "agentVersion": version,
    })
}

/// At most `max` characters.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The code to show while a link waits for approval.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
}

/// One link's device flow (link.ts `link`).
pub struct Link<'a> {
    pub http: reqwest::Client,
    pub store: &'a dyn TokenStore,
    /// `machine_info`, sent with the code request.
    pub machine: Value,
    pub target: LinkTarget,
    /// What one second of the server's polling interval is: a second, or less in tests.
    pub second: Duration,
}

impl Link<'_> {
    /// Asks `url` for a device code, shows it with `on_code`, polls until approved, and stores the refresh token.
    /// The agent's id on the cloud when it's done.
    pub async fn run(&self, url: &str, on_code: impl FnOnce(&DeviceCode)) -> Result<u64> {
        let path = self.target.device_path;
        let (ok, code) = self.post(url, &format!("{path}/code"), &self.machine).await?;
        if !ok {
            bail!("Couldn't get a code: {}", describe(&code));
        }
        let device_code = text(&code, "device_code");
        let mut interval = code
            .get("interval")
            .and_then(Value::as_u64)
            .filter(|i| *i > 0)
            .unwrap_or(5);
        on_code(&DeviceCode {
            user_code: text(&code, "user_code"),
            verification_uri: text(&code, "verification_uri"),
            verification_uri_complete: text(&code, "verification_uri_complete"),
            expires_in: code
                .get("expires_in")
                .and_then(Value::as_u64)
                .filter(|e| *e > 0)
                .unwrap_or(600),
        });
        loop {
            tokio::time::sleep(self.second * interval as u32).await;
            let body = json!({ "grant_type": DEVICE_CODE_GRANT, "device_code": device_code });
            let (ok, res) = self.post(url, &format!("{path}/token"), &body).await?;
            if ok {
                let refresh = res
                    .get("refresh_token")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty());
                let Some(refresh) = refresh else {
                    bail!("The {} approved the link but sent no refresh token.", self.target.name);
                };
                self.store.set(url, refresh)?;
                return Ok(res.get("agent_id").and_then(Value::as_u64).unwrap_or(0));
            }
            match res.get("error").and_then(Value::as_str) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += 5, // RFC 8628 section 3.5
                Some("access_denied") => bail!("The link was denied on the /link page."),
                Some("expired_token") => bail!("The code expired before it was approved. Run the link again."),
                _ => bail!("Linking failed: {}", describe(&res)),
            }
        }
    }

    /// POSTs JSON; whether it answered 2xx, and its JSON body.
    async fn post(&self, url: &str, path: &str, body: &Value) -> Result<(bool, Map<String, Value>)> {
        let res = self
            .http
            .post(format!("{url}{path}"))
            .json(body)
            .send()
            .await
            .map_err(|e| anyhow!("Couldn't reach {url}: {}. Check {}.", cause(&e), self.target.setting))?;
        let status = res.status();
        match res.json::<Value>().await {
            Ok(Value::Object(json)) => Ok((status.is_success(), json)),
            _ => bail!(
                "{url}{path} answered {} without JSON. Is {url} a Retaskable {}?",
                status.as_u16(),
                self.target.name
            ),
        }
    }
}

fn text(json: &Map<String, Value>, key: &str) -> String {
    match json.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn describe(json: &Map<String, Value>) -> String {
    ["error_description", "error", "message"]
        .iter()
        .find_map(|k| {
            json.get(*k)
                .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
        })
        .unwrap_or_else(|| Value::Object(json.clone()).to_string())
}

/// The innermost cause of a request's failure ("connection refused"), as the TypeScript agent's `causeMessage`.
pub(crate) fn cause(err: &(dyn std::error::Error + 'static)) -> String {
    let mut inner = err;
    while let Some(next) = inner.source() {
        inner = next;
    }
    inner.to_string()
}

/// An access token for one cloud, or why there's none.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenAnswer {
    Token {
        url: String,
        access_token: String,
    },
    /// Not linked: look again later.
    Idle,
    /// The cloud couldn't be reached.
    Unreachable,
}

/// An access token is used until a minute before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);

struct Held {
    access_token: String,
    until: Instant,
}

/// One link's access token, refreshed from the stored refresh token when it runs out (link.ts `DeviceTokens`).
/// Refresh tokens rotate on every use and reusing one unlinks the agent, so there's only ever one refresh under way:
/// the lock is held across it.
pub struct DeviceTokens {
    url: String,
    target: LinkTarget,
    store: Arc<dyn TokenStore>,
    http: reqwest::Client,
    held: Mutex<Option<Held>>,
}

impl DeviceTokens {
    pub fn new(url: String, target: LinkTarget, store: Arc<dyn TokenStore>, http: reqwest::Client) -> Self {
        Self {
            url,
            target,
            store,
            http,
            held: Mutex::new(None),
        }
    }

    /// The token in hand while it has a minute left, else a fresh one. `stale`: a token the caller found no good,
    /// which isn't handed out again.
    pub async fn access_token(&self, stale: Option<&str>) -> TokenAnswer {
        let mut held = self.held.lock().await;
        if let Some(h) = held.as_ref()
            && Some(h.access_token.as_str()) != stale
            && Instant::now() < h.until
        {
            return TokenAnswer::Token {
                url: self.url.clone(),
                access_token: h.access_token.clone(),
            };
        }
        *held = None;
        let (answer, fresh) = self.refresh().await;
        *held = fresh;
        answer
    }

    /// Waits up to `wait` for a refresh under way to answer and store its new refresh token. Exiting in the middle of
    /// one loses the token the server just rotated to, and the next start's replay of the old one unlinks the agent.
    /// `then` runs with no refresh able to start, to stop whatever would start one.
    pub async fn settled(&self, wait: Duration, then: impl FnOnce()) {
        let _held = tokio::time::timeout(wait, self.held.lock()).await;
        then();
    }

    /// Swaps the refresh token for an access token, storing the new refresh token at once.
    async fn refresh(&self) -> (TokenAnswer, Option<Held>) {
        let url = &self.url;
        let refresh_token = match self.store.get(url) {
            Ok(Some(token)) => token,
            Ok(None) => return (TokenAnswer::Idle, None), // not linked (yet)
            Err(e) => {
                tracing::warn!("cloud ({url}): not connecting: {e:#}");
                return (TokenAnswer::Idle, None);
            }
        };
        let body = json!({ "grant_type": "refresh_token", "refresh_token": refresh_token });
        let res = match self
            .http
            .post(format!("{url}{}/token", self.target.device_path))
            .json(&body)
            .send()
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::warn!("cloud ({url}): couldn't reach {url}: {}", cause(&e));
                return (TokenAnswer::Unreachable, None);
            }
        };
        let status = res.status();
        let json = res.json::<Value>().await.unwrap_or(Value::Null);
        let access = json.get("access_token").and_then(Value::as_str);
        let rotated = json.get("refresh_token").and_then(Value::as_str);
        if let (true, Some(access), Some(rotated)) = (status.is_success(), access, rotated) {
            if let Err(e) = self.store.set(url, rotated) {
                tracing::warn!("cloud ({url}): couldn't keep the new refresh token: {e:#}");
            }
            let seconds = json.get("expires_in").and_then(Value::as_u64).unwrap_or(0);
            let expires = Instant::now() + Duration::from_secs(seconds);
            let until = expires.checked_sub(TOKEN_MARGIN).unwrap_or(expires);
            let answer = TokenAnswer::Token {
                url: url.clone(),
                access_token: access.to_string(),
            };
            return (
                answer,
                Some(Held {
                    access_token: access.to_string(),
                    until,
                }),
            );
        }
        let said = |k: &str| json.get(k).and_then(Value::as_str);
        if said("error") == Some("invalid_grant") {
            // Unlinked on the website (or the token was reused): the stored token is no good any more.
            let why = said("error_description").unwrap_or("invalid_grant");
            tracing::warn!("cloud ({url}): not linked any more: {why}");
            if let Err(e) = self.store.delete(url) {
                tracing::warn!("cloud ({url}): couldn't delete the old token: {e:#}");
            }
            return (TokenAnswer::Idle, None);
        }
        let why = said("error_description").or_else(|| said("error")).unwrap_or("");
        tracing::warn!(
            "cloud ({url}): refreshing the access token failed: {} {why}",
            status.as_u16()
        );
        (TokenAnswer::Unreachable, None)
    }
}
