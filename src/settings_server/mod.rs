//! The computer-only settings page (agent-settings.md section 4, agent/08-http-api.md): `GET
//! /settings`, `POST /api/settings/session`, `GET`/`POST /api/settings`, `POST
//! /api/settings/restart`. Every request's Host must be this computer's own, a POST's Origin its
//! own, and a change the session's CSRF token (section 4.3); signing in is by a one-time tray
//! token, or, once network access is on, this computer's password (section 4.2).
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::api::not_found_response;
use crate::config;
use crate::network::{self, NetworkAccess};
use crate::password;
use crate::store::MachineStore;
use crate::tags;

const COOKIE_NAME: &str = "retaskable_settings_session";
const SESSION_TTL_SECS: u64 = 8 * 60 * 60;
const MAX_BODY_BYTES: usize = 16 * 1024;
const FAILURES_BEFORE_A_WAIT: u32 = 5;
const FIRST_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 60 * 60;

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

/// How `SettingsServer` reads an environment variable, injectable for tests.
type EnvFn = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone)]
struct Session {
    csrf: String,
    expires_at: u64,
}

#[derive(Default)]
struct FailureState {
    count: u32,
    window_start: u64,
    blocked_until: u64,
    wait_secs: u64,
}

#[derive(Default)]
struct InnerState {
    /// A tray-minted token, good once before it expires (section 4.2). Nothing in this agent
    /// mints one yet — the tray's own "Settings…" link isn't ported — so this stays empty
    /// outside tests until it is.
    tokens: HashMap<String, u64>,
    sessions: HashMap<String, Session>,
    failures: HashMap<IpAddr, FailureState>,
}

/// The settings page's server-side state: one instance shared (behind an `Arc`) by every request.
pub struct SettingsServer {
    dir: PathBuf,
    /// The port the agent is actually answering on, to say whether a restart is waiting and to
    /// check a request's Host (section 4.3).
    port: u16,
    /// Whether network access was on (so the port listens on every address) when the agent
    /// started, to say whether a restart is waiting the same way `port` does.
    network_enabled: bool,
    /// `RETASKABLE_NETWORK_PASSWORD`'s own access, hashed once here at construction rather than
    /// on every request: scrypt is deliberately slow, and the env var can't change while this
    /// process runs.
    env_network_access: Option<NetworkAccess>,
    env: EnvFn,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    exit: Box<dyn Fn(i32) + Send + Sync>,
    state: Mutex<InnerState>,
}

impl SettingsServer {
    pub fn new(
        dir: PathBuf,
        port: u16,
        network_enabled: bool,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        now: impl Fn() -> u64 + Send + Sync + 'static,
        exit: impl Fn(i32) + Send + Sync + 'static,
    ) -> Self {
        let env_network_access = network::env_network_access(|k| env(k));
        Self {
            dir,
            port,
            network_enabled,
            env_network_access,
            env: Box::new(env),
            now: Box::new(now),
            exit: Box::new(exit),
            state: Mutex::new(InnerState::default()),
        }
    }

    fn env(&self, key: &str) -> Option<String> {
        (self.env)(key)
    }

    /// A missing, wrong, used or old token is refused the same way (section 6): once checked,
    /// right or wrong, it's gone.
    fn verify_token(&self, token: &str) -> bool {
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        matches!(state.tokens.remove(token), Some(expires_at) if expires_at > now)
    }

    /// Matches a password against this computer's network-access hash, constant time
    /// (section 3.1), whichever set it: `RETASKABLE_NETWORK_PASSWORD` or `machine.json`. Only
    /// while network access is on (section 3.1: "Turning it off keeps the hash" so it can be
    /// turned back on, but doesn't go on signing in while it's off).
    fn verify_password(&self, password_attempt: &str) -> bool {
        let access = match &self.env_network_access {
            Some(access) => access.clone(),
            None => {
                let Ok(machine) = MachineStore::load(&self.dir) else {
                    return false;
                };
                machine.network().unwrap_or_default()
            }
        };
        access.enabled
            && access
                .password_hash
                .as_deref()
                .is_some_and(|hash| password::verify_password(password_attempt, hash))
    }

    fn create_session(&self) -> (String, String) {
        let cookie = random_hex(32);
        let csrf = random_hex(32);
        let expires_at = (self.now)() + SESSION_TTL_SECS;
        self.state.lock().unwrap().sessions.insert(
            cookie.clone(),
            Session {
                csrf: csrf.clone(),
                expires_at,
            },
        );
        (cookie, csrf)
    }

    fn authenticate(&self, headers: &HeaderMap) -> Option<Session> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        let token = parse_cookie(cookie_header, COOKIE_NAME)?;
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        let session = state.sessions.get(&token)?.clone();
        if session.expires_at <= now {
            state.sessions.remove(&token);
            return None;
        }
        Some(session)
    }

    /// `None` while `addr` may try again now; `Some(seconds)` while it must still wait
    /// (section 4.3: even a right password waits).
    fn check_rate_limit(&self, addr: IpAddr) -> Option<u64> {
        let now = (self.now)();
        let state = self.state.lock().unwrap();
        state
            .failures
            .get(&addr)
            .filter(|f| now < f.blocked_until)
            .map(|f| f.blocked_until - now)
    }

    /// Only called when `check_rate_limit` just said `addr` may try again, so a failure here is
    /// never while already blocked (section 4.3: five wrong tries within a minute wait a minute,
    /// then two, then four, up to an hour).
    fn record_failure(&self, addr: IpAddr) {
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        let entry = state.failures.entry(addr).or_insert_with(|| FailureState {
            count: 0,
            window_start: now,
            blocked_until: 0,
            wait_secs: 0,
        });
        if now.saturating_sub(entry.window_start) > 60 {
            entry.count = 0;
            entry.window_start = now;
        }
        entry.count += 1;
        if entry.count >= FAILURES_BEFORE_A_WAIT {
            entry.wait_secs = if entry.wait_secs == 0 {
                FIRST_WAIT_SECS
            } else {
                (entry.wait_secs * 2).min(MAX_WAIT_SECS)
            };
            entry.blocked_until = now + entry.wait_secs;
            entry.count = 0;
            entry.window_start = now;
        }
    }

    fn record_success(&self, addr: IpAddr) {
        self.state.lock().unwrap().failures.remove(&addr);
    }

    /// Its Host is this computer's own, at the port it answered on (section 4.3): no DNS
    /// rebinding. Network access can already make the port listen on every address (section
    /// 4.4), but this still only accepts the loopback names; widening it to the computer's own
    /// network addresses isn't ported yet (tracked outside this change).
    fn host_is_allowed(&self, host_header: &str) -> bool {
        let (host, port) = split_host_port(host_header);
        if let Some(port) = port
            && port != self.port
        {
            return false;
        }
        matches!(
            host.to_ascii_lowercase().as_str(),
            "127.0.0.1" | "localhost" | "[::1]" | "::1"
        )
    }

    fn current_settings(&self) -> Result<Value> {
        let env = |k: &str| self.env(k);
        let cfg = config::load(&self.dir)?;
        let machine = MachineStore::load(&self.dir)?;
        let facts = tags::machine_facts(env);
        let found = tags::found_tags(&facts);
        let overrides = machine.tags();
        let effective = tags::effective_tags(&found, overrides.as_ref(), env);
        let local = config::local_model(&cfg, env);
        let engine_choice = config::engine_choice(&cfg, env);
        let cursor = config::cursor_models(&cfg, env);
        let port = config::port(&cfg, env);
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
                    .map(|name| ((*name).to_string(), Value::Bool(env(name).is_some())))
                    .collect(),
            ),
        }))
    }

    /// Saves a typed tag list as changes to the found tags (added, removed), the same as setup
    /// will (agent-settings.md section 3.1).
    fn save_tags(&self, typed: &[String]) -> Result<()> {
        let env = |k: &str| self.env(k);
        let found = tags::found_tags(&tags::machine_facts(env));
        let cleaned = tags::clean_tags(Value::Array(typed.iter().cloned().map(Value::String).collect()));
        let added = cleaned.iter().filter(|t| !found.contains(t)).cloned().collect();
        let removed = found.iter().filter(|t| !cleaned.contains(t)).cloned().collect();
        let overrides = tags::tag_overrides(&found, Some(&tags::TagOverrides { added, removed }));
        let mut store = MachineStore::load(&self.dir)?;
        store.set_tags(overrides)
    }

    /// `Err` when `var` sets this setting (section 3.2): a save must refuse the same way the
    /// page greys the field out.
    fn env_gate(&self, var: &str) -> Result<(), SaveError> {
        if self.env(var).is_some() {
            return Err(SaveError::Invalid(format!("Set by {var}: change it there.")));
        }
        Ok(())
    }

    /// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." No write, so a
    /// caller can validate every field of a multi-field save before writing any of them.
    fn validate_port(&self, port: u16) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_PORT")?;
        if !config::port_in_range(port) {
            return Err(SaveError::Invalid("Port must be 1024 to 65535.".to_string()));
        }
        Ok(())
    }

    fn save_port(&self, port: u16) -> Result<(), SaveError> {
        self.validate_port(port)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert("port".to_string(), json!(port));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "How often it checks: 2 to 3600 seconds."
    fn validate_poll_interval(&self, secs: u32) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_POLL_SEC")?;
        if !config::poll_interval_in_range(secs) {
            return Err(SaveError::Invalid(
                "How often it checks must be 2 to 3600 seconds.".to_string(),
            ));
        }
        Ok(())
    }

    fn save_poll_interval(&self, secs: u32) -> Result<(), SaveError> {
        self.validate_poll_interval(secs)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert("pollIntervalSec".to_string(), json!(secs));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Local model: on or off; its model's name ...; its
    /// address, http or https, with no user or password in it."
    fn validate_local(&self, update: &LocalUpdate) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_LOCAL_MODEL")?;
        self.env_gate("RETASKABLE_LOCAL_URL")?;
        if update.enabled && !config::valid_model_name(&update.model) {
            return Err(SaveError::Invalid(
                "Local model's name must be 1 to 100 letters, digits, '.', '_', ':', '/' or '-'.".to_string(),
            ));
        }
        if !config::valid_local_url(&update.base_url) {
            return Err(SaveError::Invalid(
                "Local model's address must be http or https, with no user or password in it.".to_string(),
            ));
        }
        Ok(())
    }

    fn save_local(&self, update: &LocalUpdate) -> Result<(), SaveError> {
        self.validate_local(update)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert(
                "local".to_string(),
                json!({
                    "enabled": update.enabled,
                    "model": update.model,
                    "baseUrl": update.base_url,
                }),
            );
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Engines: a default (claude, cursor, pi or local) and, per
    /// kind of work ..., the default or one of the four", plus the per-lane override below it.
    /// `roles` and `lanes` entries are always present in the request (the page's one "Engines"
    /// save sends the whole group), so this both parses and validates them.
    fn validate_engine_choice(&self, update: &EngineChoiceUpdate) -> Result<config::EngineChoiceConfig, SaveError> {
        self.env_gate("RETASKABLE_ENGINES")?;
        let default = config::parse_engine(&update.default)
            .ok_or_else(|| SaveError::Invalid("Engine must be claude, cursor, pi or local.".to_string()))?;
        let roles = config::RoleEngines {
            planner: parse_role_engine(&update.roles.planner)?,
            implementer: parse_role_engine(&update.roles.implementer)?,
            review: parse_role_engine(&update.roles.review)?,
            drafter: parse_role_engine(&update.roles.drafter)?,
            chat: parse_role_engine(&update.roles.chat)?,
            writer: parse_role_engine(&update.roles.writer)?,
        };
        let lanes = config::lanes_of(&update.lanes)
            .ok_or_else(|| SaveError::Invalid("Lanes must be claude, cursor or default.".to_string()))?;
        Ok(config::EngineChoiceConfig {
            default: Some(default),
            roles,
            lanes,
        })
    }

    fn save_engine_choice(&self, update: &EngineChoiceUpdate) -> Result<(), SaveError> {
        let engine_choice = self.validate_engine_choice(update)?;
        config::save_machine_settings(&self.dir, |obj| {
            obj.insert("engineChoice".to_string(), engine_choice_json(&engine_choice));
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Cursor's models: one per tier ...; empty is auto."
    fn validate_cursor_models(&self, update: &CursorModelsUpdate) -> Result<CleanedCursorModels, SaveError> {
        self.env_gate("RETASKABLE_CURSOR_MODELS")?;
        let clean = |name: &Option<String>| -> Result<Option<String>, SaveError> {
            match name {
                None => Ok(None),
                Some(s) if s.trim().is_empty() => Ok(None),
                Some(s) if config::valid_cursor_model_name(s) => Ok(Some(s.clone())),
                Some(_) => Err(SaveError::Invalid(
                    "Cursor's model name must be letters, digits, '_', '.', ':', '/' or '-', not starting with '-'."
                        .to_string(),
                )),
            }
        };
        Ok(CleanedCursorModels {
            top: clean(&update.top)?,
            implementer: clean(&update.implementer)?,
            scout: clean(&update.scout)?,
        })
    }

    fn save_cursor_models(&self, update: &CursorModelsUpdate) -> Result<(), SaveError> {
        let cleaned = self.validate_cursor_models(update)?;
        config::save_machine_settings(&self.dir, |obj| {
            let mut models = Map::new();
            if let Some(t) = cleaned.top {
                models.insert("top".to_string(), json!(t));
            }
            if let Some(i) = cleaned.implementer {
                models.insert("implementer".to_string(), json!(i));
            }
            if let Some(s) = cleaned.scout {
                models.insert("scout".to_string(), json!(s));
            }
            match obj.entry("cursor".to_string()).or_insert_with(|| json!({})) {
                Value::Object(cursor) => {
                    cursor.insert("models".to_string(), Value::Object(models));
                }
                other => *other = json!({ "models": models }),
            }
        })?;
        Ok(())
    }

    /// agent-settings.md section 3.1: "Network access: on or off. On needs a password of at
    /// least 8 characters ... Turning it off keeps the hash, so turning it on again needs no new
    /// one." Reads `machine.json` when `update` needs its already-saved hash, but never writes.
    fn validate_network(&self, update: &NetworkUpdate) -> Result<(), SaveError> {
        self.env_gate("RETASKABLE_NETWORK_PASSWORD")?;
        let has_password = match &update.password {
            Some(p) if !p.is_empty() => {
                if p.encode_utf16().count() < network::MIN_PASSWORD_LEN {
                    return Err(SaveError::Invalid(format!(
                        "Password must be at least {} characters.",
                        network::MIN_PASSWORD_LEN
                    )));
                }
                true
            }
            _ => MachineStore::load(&self.dir)?
                .network()
                .is_some_and(|n| n.password_hash.is_some()),
        };
        if update.enabled && !has_password {
            return Err(SaveError::Invalid(
                "Network access needs a password of at least 8 characters.".to_string(),
            ));
        }
        Ok(())
    }

    fn save_network(&self, update: &NetworkUpdate) -> Result<(), SaveError> {
        self.validate_network(update)?;
        let mut store = MachineStore::load(&self.dir)?;
        let password_hash = match &update.password {
            Some(p) if !p.is_empty() => Some(password::hash_password(p)),
            _ => store.network().and_then(|n| n.password_hash),
        };
        store.set_network(NetworkAccess {
            enabled: update.enabled,
            password_hash,
        })?;
        Ok(())
    }
}

/// A save's refusal (section 3.1's own validation, or section 3.2's env-var gate) as opposed to
/// an unexpected I/O failure: the first answers 400, the second 500 (`save_error_response`).
enum SaveError {
    Invalid(String),
    Io(anyhow::Error),
}

impl From<anyhow::Error> for SaveError {
    fn from(e: anyhow::Error) -> Self {
        SaveError::Io(e)
    }
}

/// `engineChoice` as both `GET /api/settings` returns it and `config.json` stores it: the same
/// shape either way, so a save can write back exactly what a read would show.
fn engine_choice_json(cfg: &config::EngineChoiceConfig) -> Value {
    json!({
        "default": cfg.default.map(|e| e.to_string()),
        "roles": {
            "planner": cfg.roles.planner.map(|e| e.to_string()),
            "implementer": cfg.roles.implementer.map(|e| e.to_string()),
            "review": cfg.roles.review.map(|e| e.to_string()),
            "drafter": cfg.roles.drafter.map(|e| e.to_string()),
            "chat": cfg.roles.chat.map(|e| e.to_string()),
            "writer": cfg.roles.writer.map(|e| e.to_string()),
        },
        "lanes": cfg.lanes.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

/// A role's own override (agent-settings.md section 3.1): empty or `default` follows the default
/// engine (`None`), anything else must be one of the four engines.
fn parse_role_engine(value: &str) -> Result<Option<config::Engine>, SaveError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    config::parse_engine(trimmed)
        .map(Some)
        .ok_or_else(|| SaveError::Invalid("Engine must be claude, cursor, pi, local or default.".to_string()))
}

fn save_error_response(e: SaveError) -> Response {
    match e {
        SaveError::Invalid(message) => bad_request(&message),
        SaveError::Io(e) => internal_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalUpdate {
    enabled: bool,
    model: String,
    base_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineChoiceUpdate {
    default: String,
    #[serde(default)]
    roles: RoleEngineUpdate,
    #[serde(default)]
    lanes: Vec<String>,
}

/// `engineChoice.roles`' raw request shape: each field is the role's own engine name, `default`
/// or empty to follow the default engine (`parse_role_engine`).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RoleEngineUpdate {
    planner: String,
    implementer: String,
    review: String,
    drafter: String,
    chat: String,
    writer: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CursorModelsUpdate {
    top: Option<String>,
    implementer: Option<String>,
    scout: Option<String>,
}

/// A `CursorModelsUpdate`'s names, validated and emptied to `None` (section 3.1: "empty is auto").
struct CleanedCursorModels {
    top: Option<String>,
    implementer: Option<String>,
    scout: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkUpdate {
    enabled: bool,
    #[serde(default)]
    password: Option<String>,
}

/// The settings page's own routes (agent/08-http-api.md), merged into the agent's router
/// alongside `/api/health`.
pub fn router(settings: Arc<SettingsServer>) -> Router {
    Router::new()
        .route("/settings", get(get_settings_page))
        .route("/api/settings/session", post(post_session))
        .route("/api/settings", get(get_settings).post(post_settings))
        .route("/api/settings/restart", post(post_restart))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(settings.clone(), security_envelope))
        .with_state(settings)
}

/// Section 4.3, every request: Host is this computer's own, and a POST's Origin is its own.
/// Anything else answers the same 404 as a path this agent doesn't serve, so it gives nothing
/// away about which paths exist (agent/08-http-api.md: the port takes no command either way).
async fn security_envelope(State(settings): State<Arc<SettingsServer>>, req: Request, next: Next) -> Response {
    let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return not_found_response();
    };
    if !settings.host_is_allowed(&host) {
        return not_found_response();
    }
    if req.method() == Method::POST {
        let expected = format!("http://{host}");
        let origin_ok = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|origin| origin.eq_ignore_ascii_case(&expected));
        if !origin_ok {
            return not_found_response();
        }
    }
    next.run(req).await
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

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct SettingsUpdate {
    tags: Option<Vec<String>>,
    port: Option<u16>,
    poll_interval_sec: Option<u32>,
    local: Option<LocalUpdate>,
    engine_choice: Option<EngineChoiceUpdate>,
    cursor_models: Option<CursorModelsUpdate>,
    network: Option<NetworkUpdate>,
    #[serde(flatten)]
    unsupported: HashMap<String, Value>,
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
    // Validate every field this body touches before writing any of them, so a body with a good
    // field and a bad one (e.g. tags plus an out-of-range port) never leaves a half-done save.
    if let Some(port) = update.port
        && let Err(e) = settings.validate_port(port)
    {
        return save_error_response(e);
    }
    if let Some(secs) = update.poll_interval_sec
        && let Err(e) = settings.validate_poll_interval(secs)
    {
        return save_error_response(e);
    }
    if let Some(local) = &update.local
        && let Err(e) = settings.validate_local(local)
    {
        return save_error_response(e);
    }
    if let Some(engine_choice) = &update.engine_choice
        && let Err(e) = settings.validate_engine_choice(engine_choice)
    {
        return save_error_response(e);
    }
    if let Some(cursor_models) = &update.cursor_models
        && let Err(e) = settings.validate_cursor_models(cursor_models)
    {
        return save_error_response(e);
    }
    if let Some(net_update) = &update.network
        && let Err(e) = settings.validate_network(net_update)
    {
        return save_error_response(e);
    }

    if let Some(typed) = &update.tags
        && let Err(e) = settings.save_tags(typed)
    {
        return internal_error(&e.to_string());
    }
    if let Some(port) = update.port
        && let Err(e) = settings.save_port(port)
    {
        return save_error_response(e);
    }
    if let Some(secs) = update.poll_interval_sec
        && let Err(e) = settings.save_poll_interval(secs)
    {
        return save_error_response(e);
    }
    if let Some(local) = &update.local
        && let Err(e) = settings.save_local(local)
    {
        return save_error_response(e);
    }
    if let Some(engine_choice) = &update.engine_choice
        && let Err(e) = settings.save_engine_choice(engine_choice)
    {
        return save_error_response(e);
    }
    if let Some(cursor_models) = &update.cursor_models
        && let Err(e) = settings.save_cursor_models(cursor_models)
    {
        return save_error_response(e);
    }
    if let Some(net_update) = &update.network
        && let Err(e) = settings.save_network(net_update)
    {
        return save_error_response(e);
    }
    Json(json!({ "saved": true })).into_response()
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

fn csrf_ok(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("X-CSRF-Token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|token| constant_time_eq(token, expected))
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

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn parse_cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

/// Splits a Host header into its hostname and, when one is given, its port. An IPv6 literal
/// (`[::1]` or `[::1]:4700`) keeps its brackets as part of the hostname, same as a browser sends.
fn split_host_port(host_header: &str) -> (&str, Option<u16>) {
    if let Some(after_bracket) = host_header.strip_prefix('[') {
        return match after_bracket.find(']') {
            Some(end) => {
                let host = &host_header[..end + 2];
                let port = after_bracket[end + 1..].strip_prefix(':').and_then(|p| p.parse().ok());
                (host, port)
            }
            None => (host_header, None),
        };
    }
    match host_header.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => (host, port.parse().ok()),
        _ => (host_header, None),
    }
}

fn random_hex(bytes_len: usize) -> String {
    (0..bytes_len)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

/// Shared by this module's own tests and `api`'s (a `SettingsServer` behind the same port the
/// tests' requests use as their Host).
#[cfg(test)]
pub(crate) fn test_server() -> (Arc<SettingsServer>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        false,
        |_: &str| None,
        || 1_700_000_000,
        |_: i32| {},
    ));
    (settings, dir)
}

#[cfg(test)]
mod tests;
