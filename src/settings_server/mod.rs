//! The computer-only settings page (agent-settings.md section 4, agent/08-http-api.md): `GET
//! /settings`, `POST /api/settings/session`, `GET`/`POST /api/settings`, `POST
//! /api/settings/restart`. Every request's Host must be this computer's own, a POST's Origin its
//! own, and a change the session's CSRF token (section 4.3); signing in is by a one-time tray
//! token, or, once network access is on, this computer's password (section 4.2).
use crate::env::Env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config;
use crate::link::{self, TokenStore};
use crate::network::{self, NetworkAccess};
use crate::profile;
use crate::store::MachineStore;
use crate::tags;

mod auth;
mod save;
mod status;
use auth::{COOKIE_NAME, InnerState, SESSION_TTL_SECS, csrf_ok, random_hex, security_envelope};
use save::{SettingsUpdate, engine_choice_json, save_error_response};
pub use status::StatusSource;

const MAX_BODY_BYTES: usize = 16 * 1024;

/// agent-settings.md section 3.2: the environment variables the settings page greys out.
const AGENT_ENV_VARS: [&str; 8] = [
    "RETASKABLE_PORT",
    "RETASKABLE_POLL_SEC",
    "RETASKABLE_TAGS",
    "RETASKABLE_LOCAL_MODEL",
    "RETASKABLE_LOCAL_URL",
    "RETASKABLE_ENGINES",
    "RETASKABLE_CURSOR_MODELS",
    "RETASKABLE_NETWORK_PASSWORD",
];

static SETTINGS_HTML: &str = include_str!("../../assets/settings.html");

/// The settings page's server-side state: one instance shared (behind an `Arc`) by every request.
pub struct SettingsServer {
    dir: PathBuf,
    /// The port the agent is actually answering on, to say whether a restart is waiting and to
    /// check a request's Host (section 4.3).
    port: u16,
    /// This build's own default port (installers/03-the-bundle-agent-bundle.md section 3.13: a
    /// Staging release build's own 4701), used once neither `RETASKABLE_PORT` nor the config's
    /// own `port` says a valid one, so a config re-read agrees with how `port` above was found.
    default_port: u16,
    /// Whether network access was on (so the port listens on every address) when the agent
    /// started, to say whether a restart is waiting the same way `port` does.
    network_enabled: bool,
    /// `RETASKABLE_NETWORK_PASSWORD`'s own access, hashed once here at construction rather than
    /// on every request: scrypt is deliberately slow, and the env var can't change while this
    /// process runs.
    env_network_access: Option<NetworkAccess>,
    env: Env,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    exit: Box<dyn Fn(i32) + Send + Sync>,
    /// This machine's stored cloud tokens, for `GET /api/link` (section 9.2's "computer's link
    /// state", local only).
    keychain: Arc<dyn TokenStore>,
    state: Mutex<InnerState>,
    /// The Now tab's live inputs (`status.rs`), given once at start.
    status: OnceLock<StatusSource>,
}

impl SettingsServer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dir: PathBuf,
        port: u16,
        default_port: u16,
        network_enabled: bool,
        env: Env,
        now: impl Fn() -> u64 + Send + Sync + 'static,
        exit: impl Fn(i32) + Send + Sync + 'static,
        keychain: Arc<dyn TokenStore>,
    ) -> Self {
        let env_network_access = network::env_network_access(&env);
        Self {
            dir,
            port,
            default_port,
            network_enabled,
            env_network_access,
            env,
            now: Box::new(now),
            exit: Box::new(exit),
            keychain,
            state: Mutex::new(InnerState::default()),
            status: OnceLock::new(),
        }
    }

    fn current_settings(&self) -> Result<Value> {
        let env = &self.env;
        let cfg = config::load(&self.dir)?;
        let machine = MachineStore::load(&self.dir)?;
        let facts = tags::machine_facts(env);
        let found = tags::found_tags(&facts);
        let overrides = machine.tags();
        let effective = tags::effective_tags(&found, overrides.as_ref(), env);
        let local = config::local_model(&cfg, env);
        let engine_choice = config::engine_choice(&cfg, env);
        let cursor = config::cursor_models(&cfg, env);
        let port = config::port(&cfg, env, self.default_port);
        let lanes = config::lanes(&cfg, env);
        let net = self
            .env_network_access
            .clone()
            .unwrap_or_else(|| machine.network().unwrap_or_default());
        Ok(json!({
            "port": port,
            // agent-settings.md section 4.1: whether the saved port or network access differs
            // from what the agent runs on.
            "restartWaiting": port != self.port || net.enabled != self.network_enabled,
            "pollIntervalSec": config::poll_interval_sec(&cfg, env),
            "lanes": lanes,
            "tags": {
                "found": found,
                "added": overrides.as_ref().map(|o| o.added.clone()).unwrap_or_default(),
                "removed": overrides.as_ref().map(|o| o.removed.clone()).unwrap_or_default(),
                "effective": effective,
            },
            "local": {
                "enabled": local.enabled,
                "model": local.model,
                "baseUrl": local.base_url,
            },
            "engineChoice": engine_choice_json(&engine_choice),
            "cursorModels": {
                "top": cursor.top,
                "implementer": cursor.implementer,
                "scout": cursor.scout,
            },
            "network": {
                "enabled": net.enabled,
                "hasPassword": net.password_hash.is_some(),
            },
            "envVars": Value::Object(
                AGENT_ENV_VARS
                    .iter()
                    .map(|name| ((*name).to_string(), Value::Bool(env.get(name).is_some())))
                    .collect(),
            ),
        }))
    }

    /// `GET /api/link`'s view of each of retaskable.com's three environments, and a custom
    /// `cloud.url` when one is set (tray.md section 4.2's "Link to" submenu), with whether this
    /// machine already holds a stored token for it. The in-progress "Waiting: code" state isn't
    /// here: only the `retaskable link` terminal command tracks it, and nothing persists it.
    fn link_view(&self) -> Result<Value> {
        let env = &self.env;
        let cfg = config::load(&self.dir)?;
        let environments: Vec<Value> = link::cloud_urls(&cfg, env, None)
            .into_iter()
            .map(|url| {
                let connected = matches!(self.keychain.get(&url), Ok(Some(_)));
                json!({ "name": link::environment_name(&url), "url": url, "connected": connected })
            })
            .collect();
        Ok(json!({ "environments": environments }))
    }
}

/// The 404 for a path this agent doesn't serve. A request the Host/Origin checks refuse gets the
/// same one, so it gives away nothing about which paths exist.
pub(crate) fn not_found_response() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response()
}

/// The settings page's own routes (agent/08-http-api.md), merged into the agent's router
/// alongside `/api/health`.
pub fn router(settings: Arc<SettingsServer>) -> Router {
    Router::new()
        .route("/settings", get(get_settings_page))
        .route("/api/settings/session", post(post_session))
        .route("/api/settings", get(get_settings).post(post_settings))
        .route("/api/settings/status", get(status::get_status))
        .route("/api/settings/restart", post(post_restart))
        .route("/api/link", get(get_link))
        .route("/api/profile", get(get_profile))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(settings.clone(), security_envelope))
        .with_state(settings)
}

async fn get_settings_page() -> Response {
    let nonce = random_hex(16);
    let html = SETTINGS_HTML.replace("{{NONCE}}", &nonce);
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'"
    );
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CONTENT_SECURITY_POLICY, csp),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::X_FRAME_OPTIONS, "DENY".to_string()),
        ],
        html,
    )
        .into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SessionRequest {
    token: Option<String>,
    password: Option<String>,
}

async fn post_session(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: axum::body::Bytes,
) -> Response {
    let addr = addr.ip();
    if let Some(wait_secs) = settings.check_rate_limit(addr) {
        return too_many_requests(wait_secs);
    }
    let Ok(req) = serde_json::from_slice::<SessionRequest>(&body) else {
        settings.record_failure(addr);
        return bad_request("No JSON body.");
    };
    let signed_in = match (&req.token, &req.password) {
        (Some(token), _) => settings.verify_token(token),
        (None, Some(password)) => settings.verify_password(password),
        (None, None) => false,
    };
    if !signed_in {
        settings.record_failure(addr);
        return unauthorized("Wrong or expired token or password.");
    }
    settings.record_success(addr);
    let (cookie, csrf) = settings.create_session();
    let set_cookie = format!("{COOKIE_NAME}={cookie}; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL_SECS}; Path=/");
    ([(header::SET_COOKIE, set_cookie)], Json(json!({ "csrfToken": csrf }))).into_response()
}

async fn get_settings(State(settings): State<Arc<SettingsServer>>, headers: HeaderMap) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    match settings.current_settings() {
        Ok(value) => Json(value).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn post_settings(
    State(settings): State<Arc<SettingsServer>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(session) = settings.authenticate(&headers) else {
        return unauthorized("Sign in first.");
    };
    if !csrf_ok(&headers, &session.csrf) {
        return unauthorized("Missing or wrong CSRF token.");
    }
    let Ok(update) = serde_json::from_slice::<SettingsUpdate>(&body) else {
        return bad_request("No JSON body.");
    };
    if !update.unsupported.is_empty() {
        let mut names: Vec<&String> = update.unsupported.keys().collect();
        names.sort();
        let names = names.into_iter().map(String::as_str).collect::<Vec<_>>().join(", ");
        return bad_request(&format!("Not a setting this agent can save yet: {names}."));
    }
    match settings.apply(&update) {
        Ok(()) => Json(json!({ "saved": true })).into_response(),
        Err(e) => save_error_response(e),
    }
}

async fn get_link(State(settings): State<Arc<SettingsServer>>, headers: HeaderMap) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    match settings.link_view() {
        Ok(value) => Json(value).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// `GET /api/profile?fresh=1`: every read already calls `gh api user` fresh (no cache here for
/// `fresh` to bypass), so the query string changes nothing and isn't inspected.
async fn get_profile(State(settings): State<Arc<SettingsServer>>, headers: HeaderMap) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    match profile::read() {
        Ok(p) => Json(json!({
            "signedIn": true,
            "login": p.login,
            "name": p.name,
            "avatarUrl": p.avatar_url,
        }))
        .into_response(),
        Err(message) => Json(json!({ "signedIn": false, "error": message })).into_response(),
    }
}

async fn post_restart(State(settings): State<Arc<SettingsServer>>, headers: HeaderMap) -> Response {
    let Some(session) = settings.authenticate(&headers) else {
        return unauthorized("Sign in first.");
    };
    if !csrf_ok(&headers, &session.csrf) {
        return unauthorized("Missing or wrong CSRF token.");
    }
    (settings.exit)(75);
    Json(json!({ "restarting": true })).into_response()
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn bad_request(message: &str) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

fn unauthorized(message: &str) -> Response {
    json_error(StatusCode::UNAUTHORIZED, message)
}

fn internal_error(message: &str) -> Response {
    json_error(StatusCode::INTERNAL_SERVER_ERROR, message)
}

fn too_many_requests(wait_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, wait_secs.to_string())],
        Json(json!({ "error": "Too many wrong tries. Wait and try again." })),
    )
        .into_response()
}

/// Shared by this module's own tests and `api`'s (a `SettingsServer` behind the same port the
/// tests' requests use as their Host).
#[cfg(test)]
pub(crate) fn test_server() -> (Arc<SettingsServer>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let keychain = test_keychain(dir.path());
    (test_server_with_keychain(dir.path(), keychain), dir)
}

/// [`test_server`] with the keychain a caller provides, for a test that cares what this machine
/// is linked to.
#[cfg(test)]
pub(crate) fn test_server_with_keychain(dir: &std::path::Path, keychain: Arc<dyn TokenStore>) -> Arc<SettingsServer> {
    Arc::new(SettingsServer::new(
        dir.to_path_buf(),
        4700,
        4700,
        false,
        Env::empty(),
        || 1_700_000_000,
        |_: i32| {},
        keychain,
    ))
}

/// A `FileStore` under `dir`, for a test that doesn't care what this machine is linked to.
#[cfg(test)]
pub(crate) fn test_keychain(dir: &std::path::Path) -> Arc<dyn TokenStore> {
    Arc::new(link::FileStore::new(
        dir.join("credentials.json"),
        "retaskable-agent-test",
    ))
}

#[cfg(test)]
mod tests;
