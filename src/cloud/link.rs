//! The device-authorization flow a machine uses to link to a cloud (cloud/04-authentication.md
//! section 4.2): `POST /api/device/code` gets a user code to show, then `POST /api/device/token`
//! is polled until the person has approved it on the site, denied it, or the code expired.
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// `POST /api/device/code`'s response (cloud/04-authentication.md section 4.2 step 1).
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// `POST /api/device/token`'s response once the person has approved the code (step 4's success
/// case).
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    pub refresh_token: String,
    pub agent_id: String,
}

/// Everything that can stop a link attempt.
#[derive(Debug)]
pub enum LinkError {
    /// The cloud couldn't be reached at all (DNS, connection refused, timeout): never guessed at
    /// as a denial or an expiry.
    Unreachable(String),
    /// `error: "access_denied"`: the person said no.
    Denied,
    /// `error: "expired_token"`, or this machine's own deadline passed first.
    Expired,
    /// Any other OAuth error code the cloud sent back.
    Other { error: String, description: Option<String> },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Unreachable(msg) => write!(f, "couldn't reach the cloud: {msg}"),
            LinkError::Denied => write!(f, "the link was denied"),
            LinkError::Expired => write!(f, "the code expired before it was approved"),
            LinkError::Other { error, description } => match description {
                Some(d) => write!(f, "{error}: {d}"),
                None => write!(f, "{error}"),
            },
        }
    }
}

impl std::error::Error for LinkError {}

#[derive(Debug, Serialize)]
struct CodeRequest<'a> {
    machine: &'a str,
    os: &'a str,
    #[serde(rename = "agentVersion")]
    agent_version: &'a str,
}

#[derive(Debug, Deserialize, Default)]
struct ErrorBody {
    error: Option<String>,
    error_description: Option<String>,
}

/// What one `/api/device/token` poll found.
enum PollOutcome {
    Pending,
    SlowDown,
    Approved(DeviceToken),
}

/// Talks to one cloud's device-authorization endpoints.
pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    async fn request_code(&self, machine: &str, os: &str, agent_version: &str) -> Result<DeviceCode, LinkError> {
        let resp = self
            .http
            .post(format!("{}/api/device/code", self.base_url))
            .json(&CodeRequest {
                machine,
                os,
                agent_version,
            })
            .send()
            .await
            .map_err(|e| LinkError::Unreachable(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(error_from_body(resp).await);
        }
        resp.json().await.map_err(|e| LinkError::Unreachable(e.to_string()))
    }

    async fn poll_token(&self, device_code: &str) -> Result<PollOutcome, LinkError> {
        let resp = self
            .http
            .post(format!("{}/api/device/token", self.base_url))
            .json(&serde_json::json!({ "device_code": device_code }))
            .send()
            .await
            .map_err(|e| LinkError::Unreachable(e.to_string()))?;

        if resp.status().is_success() {
            let token = resp.json().await.map_err(|e| LinkError::Unreachable(e.to_string()))?;
            return Ok(PollOutcome::Approved(token));
        }

        let status = resp.status();
        let body: ErrorBody = resp.json().await.unwrap_or_default();
        match body.error.as_deref() {
            Some("authorization_pending") => Ok(PollOutcome::Pending),
            Some("slow_down") => Ok(PollOutcome::SlowDown),
            Some("access_denied") => Err(LinkError::Denied),
            Some("expired_token") => Err(LinkError::Expired),
            Some(other) => Err(LinkError::Other {
                error: other.to_string(),
                description: body.error_description,
            }),
            None => Err(LinkError::Other {
                error: status.to_string(),
                description: body.error_description,
            }),
        }
    }
}

async fn error_from_body(resp: reqwest::Response) -> LinkError {
    let status = resp.status();
    let body: ErrorBody = resp.json().await.unwrap_or_default();
    LinkError::Other {
        error: body.error.unwrap_or_else(|| status.to_string()),
        description: body.error_description,
    }
}

/// Requests a code, hands it to `on_code` once (to show the person where to go and what to
/// type), then polls `/api/device/token` until it's approved, denied, or expired.
///
/// `interval` grows by 5 seconds on every `slow_down`, per cloud/04-authentication.md section
/// 4.2 step 3. A local deadline backs up the server's own `expired_token`, in case the server
/// never says so itself.
pub async fn link(
    client: &Client,
    machine: &str,
    os: &str,
    agent_version: &str,
    on_code: impl FnOnce(&DeviceCode),
) -> Result<DeviceToken, LinkError> {
    let code = client.request_code(machine, os, agent_version).await?;
    on_code(&code);

    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = Duration::from_secs(code.interval);

    loop {
        if Instant::now() >= deadline {
            return Err(LinkError::Expired);
        }
        tokio::time::sleep(interval).await;
        match client.poll_token(&code.device_code).await? {
            PollOutcome::Approved(token) => return Ok(token),
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval += Duration::from_secs(5),
        }
    }
}

/// `RETASKABLE_MACHINE`, else `hostname()`, else a last-resort fallback: the device-code
/// request's `machine` field is shown to the person on the site (cloud/04-authentication.md
/// section 4.2 step 2), so it's never left empty.
pub fn machine_name(env: impl Fn(&str) -> Option<String>, hostname: impl Fn() -> Option<String>) -> String {
    env("RETASKABLE_MACHINE")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(hostname)
        .unwrap_or_else(|| "unknown-machine".to_string())
}

/// The device-code request's `os` field: `std::env::consts::OS` mapped to the name people would
/// recognize. A release-version suffix (the spec's example is "Windows 10.0.26200") is left for
/// a later slice: it needs either a new crate or shelling out per platform, for a field the API
/// only needs to be 1-100 characters, non-empty.
pub fn os_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::net::TcpListener;

    struct FakeCloud {
        polls_before_approval: AtomicU32,
        slow_downs: u32,
        deny: bool,
        expire: bool,
    }

    async fn code_handler() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "device_code": "dc-1",
            "user_code": "ABCD-1234",
            "verification_uri": "https://retaskable.com/link",
            "verification_uri_complete": "https://retaskable.com/link?code=ABCD-1234",
            "expires_in": 60,
            "interval": 0,
        }))
    }

    async fn token_handler(State(state): State<Arc<FakeCloud>>) -> (axum::http::StatusCode, Json<serde_json::Value>) {
        use axum::http::StatusCode;
        if state.expire {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "expired_token" })),
            );
        }
        if state.deny {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "access_denied" })),
            );
        }
        let seen = state.polls_before_approval.fetch_add(1, Ordering::SeqCst);
        if seen < state.slow_downs {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "slow_down" })),
            );
        }
        if seen < state.slow_downs + 1 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "authorization_pending" })),
            );
        }
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "access_token": "at-1",
                "token_type": "Bearer",
                "expires_in": 900,
                "refresh_token": "rt-1",
                "agent_id": "agent-1",
            })),
        )
    }

    async fn fake_cloud(state: FakeCloud) -> String {
        let app = Router::new()
            .route("/api/device/code", post(code_handler))
            .route("/api/device/token", post(token_handler))
            .with_state(Arc::new(state));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn requesting_a_code_returns_what_the_cloud_sent() {
        let base = fake_cloud(FakeCloud {
            polls_before_approval: AtomicU32::new(0),
            slow_downs: 0,
            deny: false,
            expire: false,
        })
        .await;
        let client = Client::new(base);
        let code = client.request_code("my-machine", "Windows", "0.1.0").await.unwrap();
        assert_eq!(code.user_code, "ABCD-1234");
        assert_eq!(code.device_code, "dc-1");
    }

    #[tokio::test]
    async fn an_unreachable_cloud_is_unreachable_not_a_guess() {
        let client = Client::new("http://127.0.0.1:1");
        let err = client.request_code("m", "Linux", "0.1.0").await.unwrap_err();
        assert!(matches!(err, LinkError::Unreachable(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn link_calls_on_code_once_and_returns_the_token_once_approved() {
        let base = fake_cloud(FakeCloud {
            polls_before_approval: AtomicU32::new(0),
            slow_downs: 0,
            deny: false,
            expire: false,
        })
        .await;
        let client = Client::new(base);
        let mut calls = 0;
        let token = link(&client, "my-machine", "Windows", "0.1.0", |_code| calls += 1)
            .await
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(token.agent_id, "agent-1");
        assert_eq!(token.refresh_token, "rt-1");
    }

    #[tokio::test]
    async fn slow_down_is_tolerated_and_still_reaches_approval() {
        let base = fake_cloud(FakeCloud {
            polls_before_approval: AtomicU32::new(0),
            slow_downs: 1,
            deny: false,
            expire: false,
        })
        .await;
        let client = Client::new(base);
        let token = link(&client, "my-machine", "Windows", "0.1.0", |_| {}).await.unwrap();
        assert_eq!(token.agent_id, "agent-1");
    }

    #[tokio::test]
    async fn link_stops_on_access_denied() {
        let base = fake_cloud(FakeCloud {
            polls_before_approval: AtomicU32::new(0),
            slow_downs: 0,
            deny: true,
            expire: false,
        })
        .await;
        let client = Client::new(base);
        let err = link(&client, "my-machine", "Windows", "0.1.0", |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, LinkError::Denied), "got {err:?}");
    }

    #[tokio::test]
    async fn link_stops_on_expired_token() {
        let base = fake_cloud(FakeCloud {
            polls_before_approval: AtomicU32::new(0),
            slow_downs: 0,
            deny: false,
            expire: true,
        })
        .await;
        let client = Client::new(base);
        let err = link(&client, "my-machine", "Windows", "0.1.0", |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, LinkError::Expired), "got {err:?}");
    }

    #[test]
    fn machine_name_prefers_the_env_var_then_the_hostname_then_falls_back() {
        let env = |k: &str| (k == "RETASKABLE_MACHINE").then(|| "from-env".to_string());
        assert_eq!(machine_name(env, || Some("from-host".to_string())), "from-env");

        let no_env = |_: &str| None;
        assert_eq!(machine_name(no_env, || Some("from-host".to_string())), "from-host");
        assert_eq!(machine_name(no_env, || None), "unknown-machine");

        let blank_env = |k: &str| (k == "RETASKABLE_MACHINE").then(|| "   ".to_string());
        assert_eq!(machine_name(blank_env, || Some("from-host".to_string())), "from-host");
    }

    #[test]
    fn os_name_is_never_empty() {
        assert!(!os_name().is_empty());
    }
}
