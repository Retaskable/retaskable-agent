//! The computer-only settings page (agent-settings.md section 4, agent/08-http-api.md): `GET
//! /settings`, `POST /api/settings/session`, `GET`/`POST /api/settings`, `POST
//! /api/settings/restart`. Every request's Host must be this computer's own, a POST's Origin its
//! own, and a change the session's CSRF token (section 4.3); signing in is by a one-time tray
//! token today (`verify_password` always refuses, since network access and its password aren't
//! saved by this agent yet — section 3.1).
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
use serde_json::{Value, json};

use crate::api::not_found_response;
use crate::config;
use crate::store::MachineStore;
use crate::tags;

const COOKIE_NAME: &str = "retaskable_settings_session";
const SESSION_TTL_SECS: u64 = 8 * 60 * 60;
const MAX_BODY_BYTES: usize = 16 * 1024;
const FAILURES_BEFORE_A_WAIT: u32 = 5;
const FIRST_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 60 * 60;

/// agent-settings.md section 3.2: the environment variables the settings page greys out.
const AGENT_ENV_VARS: [&str; 7] = [
    "RETASKABLE_PORT",
    "RETASKABLE_POLL_SEC",
    "RETASKABLE_TAGS",
    "RETASKABLE_LOCAL_MODEL",
    "RETASKABLE_LOCAL_URL",
    "RETASKABLE_ENGINES",
    "RETASKABLE_CURSOR_MODELS",
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
    env: EnvFn,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    exit: Box<dyn Fn(i32) + Send + Sync>,
    state: Mutex<InnerState>,
}

impl SettingsServer {
    pub fn new(
        dir: PathBuf,
        port: u16,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        now: impl Fn() -> u64 + Send + Sync + 'static,
        exit: impl Fn(i32) + Send + Sync + 'static,
    ) -> Self {
        Self {
            dir,
            port,
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

    /// Network access and its password aren't saved by this agent yet (section 3.1), so there is
    /// never a password to match and every attempt is wrong.
    fn verify_password(&self, _password: &str) -> bool {
        false
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
        if entry.wait_secs == 0 && now.saturating_sub(entry.window_start) > 60 {
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
        }
    }

    fn record_success(&self, addr: IpAddr) {
        self.state.lock().unwrap().failures.remove(&addr);
    }

    /// Its Host is this computer's own, at the port it answered on (section 4.3): no DNS
    /// rebinding. Network access and its wider set of allowed addresses aren't ported yet, since
    /// the port only ever binds `127.0.0.1` so far (section 4.4).
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
        Ok(json!({
            "port": port,
            // agent-settings.md section 4.1: whether the saved port or network access differs
            // from what the agent runs on. Network access isn't saved yet, so only the port can
            // make this true today.
            "restartWaiting": port != self.port,
            "pollIntervalSec": config::poll_interval_sec(&cfg, env),
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
            "engineChoice": {
                "default": engine_choice.default.map(|e| e.to_string()),
            },
            "cursorModels": {
                "top": cursor.top,
                "implementer": cursor.implementer,
                "scout": cursor.scout,
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
    /// will (agent-settings.md section 3.1). Port, local model, how often it checks, engines and
    /// Cursor's models have no save path yet: `config.json` has none to write to (tracked in
    /// `remaining`, not here).
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
struct SettingsUpdate {
    #[serde(default)]
    tags: Option<Vec<String>>,
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
    if let Some(typed) = &update.tags
        && let Err(e) = settings.save_tags(typed)
    {
        return internal_error(&e.to_string());
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

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    const HOST: &str = "127.0.0.1:4700";

    fn test_server() -> (Arc<SettingsServer>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            |_: &str| None,
            || 1_700_000_000,
            |_: i32| {},
        ));
        (settings, dir)
    }

    fn app(settings: Arc<SettingsServer>) -> Router {
        router(settings).layer(MockConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))))
    }

    fn request(method: &str, path: &str) -> axum::http::request::Builder {
        Request::builder().method(method).uri(path).header("Host", HOST)
    }

    async fn json_body(res: Response) -> Value {
        let body = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    fn mint_token(settings: &SettingsServer, good_for_secs: u64) -> String {
        let token = random_hex(16);
        let expires_at = (settings.now)() + good_for_secs;
        settings.state.lock().unwrap().tokens.insert(token.clone(), expires_at);
        token
    }

    #[tokio::test]
    async fn the_settings_page_has_its_csp_and_a_fresh_nonce_each_time() {
        let (settings, _dir) = test_server();
        let res = app(settings)
            .oneshot(request("GET", "/settings").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let csp = res
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(res.headers().get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
        let body = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
        assert!(!body.contains("{{NONCE}}"));
        let nonce = csp.split("nonce-").nth(1).unwrap().split('\'').next().unwrap();
        assert!(body.contains(nonce));
    }

    #[tokio::test]
    async fn a_wrong_host_is_a_404_like_any_other_path() {
        let (settings, _dir) = test_server();
        let res = app(settings)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/settings")
                    .header("Host", "evil.example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(res).await, json!({ "error": "Not found" }));
    }

    #[tokio::test]
    async fn a_host_at_the_wrong_port_is_a_404() {
        let (settings, _dir) = test_server();
        let res = app(settings)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/settings")
                    .header("Host", "127.0.0.1:9999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_post_with_no_origin_or_the_wrong_one_is_a_404() {
        let (settings, _dir) = test_server();
        let app = app(settings);
        for origin in [None, Some("http://evil.example.com")] {
            let mut builder = request("POST", "/api/settings/restart");
            if let Some(origin) = origin {
                builder = builder.header("Origin", origin);
            }
            let res = app
                .clone()
                .oneshot(builder.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn a_post_with_its_own_origin_passes_the_envelope() {
        let (settings, _dir) = test_server();
        let res = app(settings)
            .oneshot(
                request("POST", "/api/settings/session")
                    .header("Origin", format!("http://{HOST}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"token":"nope"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Past the envelope, refused for the wrong token instead of 404'd for a bad Origin.
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    fn post_body(path: &str, body: &str) -> Request<Body> {
        request("POST", path)
            .header("Origin", format!("http://{HOST}"))
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_missing_wrong_used_or_old_token_is_refused() {
        let (settings, _dir) = test_server();
        let app = app(settings.clone());

        let no_token = app
            .clone()
            .oneshot(post_body("/api/settings/session", "{}"))
            .await
            .unwrap();
        assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let old = mint_token(&settings, 0); // already expired
        let old_res = app
            .clone()
            .oneshot(post_body("/api/settings/session", &json!({ "token": old }).to_string()))
            .await
            .unwrap();
        assert_eq!(old_res.status(), StatusCode::UNAUTHORIZED);

        let used = mint_token(&settings, 300);
        let body = json!({ "token": used }).to_string();
        let first = app
            .clone()
            .oneshot(post_body("/api/settings/session", &body))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let second = app
            .clone()
            .oneshot(post_body("/api/settings/session", &body))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_good_token_signs_in_with_a_cookie_and_a_csrf_token() {
        let (settings, _dir) = test_server();
        let app = app(settings.clone());
        let token = mint_token(&settings, 300);
        let res = app
            .oneshot(post_body(
                "/api/settings/session",
                &json!({ "token": token }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(cookie.starts_with(&format!("{COOKIE_NAME}=")));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains(&format!("Max-Age={SESSION_TTL_SECS}")));
        let body = json_body(res).await;
        assert!(body["csrfToken"].is_string());
    }

    async fn sign_in(app: &Router, settings: &SettingsServer) -> (String, String) {
        let token = mint_token(settings, 300);
        let res = app
            .clone()
            .oneshot(post_body(
                "/api/settings/session",
                &json!({ "token": token }).to_string(),
            ))
            .await
            .unwrap();
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let csrf = json_body(res).await["csrfToken"].as_str().unwrap().to_string();
        (cookie, csrf)
    }

    #[tokio::test]
    async fn settings_without_a_session_are_refused() {
        let (settings, _dir) = test_server();
        let res = app(settings)
            .oneshot(request("GET", "/api/settings").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn signed_in_settings_mirror_the_found_tags_and_say_what_is_set_by_env() {
        let dir = tempfile::tempdir().unwrap();
        let env = |k: &str| (k == "RETASKABLE_TAGS").then(|| "linux".to_string());
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            env,
            || 1_700_000_000,
            |_| {},
        ));
        let app = app(settings.clone());
        let (cookie, _csrf) = sign_in(&app, &settings).await;
        let res = app
            .oneshot(
                request("GET", "/api/settings")
                    .header("Cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = json_body(res).await;
        assert_eq!(body["tags"]["effective"], json!(["linux"]));
        assert_eq!(body["envVars"]["RETASKABLE_TAGS"], json!(true));
        assert_eq!(body["envVars"]["RETASKABLE_PORT"], json!(false));
    }

    #[tokio::test]
    async fn saving_tags_without_a_csrf_token_is_refused() {
        let (settings, _dir) = test_server();
        let app = app(settings.clone());
        let (cookie, _csrf) = sign_in(&app, &settings).await;
        let res = app
            .oneshot(
                request("POST", "/api/settings")
                    .header("Origin", format!("http://{HOST}"))
                    .header("Cookie", cookie)
                    .body(Body::from(r#"{"tags":["games"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn saving_a_typed_tag_list_keeps_only_what_changes_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            |_: &str| None,
            || 1_700_000_000,
            |_: i32| {},
        ));
        let app = app(settings.clone());
        let (cookie, csrf) = sign_in(&app, &settings).await;
        let found = tags::found_tags(&tags::machine_facts(|_: &str| None));
        let mut typed = found.clone();
        typed.push("games".to_string());
        let res = app
            .clone()
            .oneshot(
                request("POST", "/api/settings")
                    .header("Origin", format!("http://{HOST}"))
                    .header("Cookie", cookie.clone())
                    .header("X-CSRF-Token", csrf.clone())
                    .body(Body::from(json!({ "tags": typed }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let saved = MachineStore::load(dir.path()).unwrap().tags().unwrap();
        assert_eq!(saved.added, vec!["games".to_string()]);
        assert!(saved.removed.is_empty());

        let get_res = app
            .oneshot(
                request("GET", "/api/settings")
                    .header("Cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = json_body(get_res).await;
        let mut effective = body["tags"]["effective"].as_array().unwrap().clone();
        effective.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        let mut expected: Vec<Value> = typed.iter().cloned().map(Value::String).collect();
        expected.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        assert_eq!(effective, expected);
    }

    #[tokio::test]
    async fn an_unsupported_setting_is_refused_by_name() {
        let (settings, _dir) = test_server();
        let app = app(settings.clone());
        let (cookie, csrf) = sign_in(&app, &settings).await;
        let res = app
            .oneshot(
                request("POST", "/api/settings")
                    .header("Origin", format!("http://{HOST}"))
                    .header("Cookie", cookie)
                    .header("X-CSRF-Token", csrf)
                    .body(Body::from(r#"{"port":4800}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = json_body(res).await;
        assert!(body["error"].as_str().unwrap().contains("port"));
    }

    #[tokio::test]
    async fn restart_calls_exit_with_75_once_signed_in_with_csrf() {
        let dir = tempfile::tempdir().unwrap();
        let exits: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
        let exits_clone = exits.clone();
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            |_: &str| None,
            || 1_700_000_000,
            move |code: i32| exits_clone.lock().unwrap().push(code),
        ));
        let app = app(settings.clone());
        let (cookie, csrf) = sign_in(&app, &settings).await;
        let res = app
            .oneshot(
                request("POST", "/api/settings/restart")
                    .header("Origin", format!("http://{HOST}"))
                    .header("Cookie", cookie)
                    .header("X-CSRF-Token", csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(*exits.lock().unwrap(), vec![75]);
    }

    #[tokio::test]
    async fn five_wrong_tries_wait_a_minute_then_longer_even_for_the_right_one() {
        let now = Arc::new(Mutex::new(1_700_000_000u64));
        let now_clone = now.clone();
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            |_: &str| None,
            move || *now_clone.lock().unwrap(),
            |_: i32| {},
        ));
        let app = app(settings.clone());

        for _ in 0..5 {
            let res = app
                .clone()
                .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        // Even a right token waits while blocked.
        let good = mint_token(&settings, 300);
        let blocked = app
            .clone()
            .oneshot(post_body(
                "/api/settings/session",
                &json!({ "token": good.clone() }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(blocked.headers().get(header::RETRY_AFTER).unwrap(), "60");

        *now.lock().unwrap() += 60;
        let after_first_wait = app
            .clone()
            .oneshot(post_body(
                "/api/settings/session",
                &json!({ "token": good }).to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(after_first_wait.status(), StatusCode::OK);

        // The good sign-in cleared the streak, so a fresh run of 5 wrong tries waits a minute
        // again rather than picking up where the last block left off.
        for _ in 0..5 {
            app.clone()
                .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
                .await
                .unwrap();
        }
        let blocked_again = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(blocked_again.headers().get(header::RETRY_AFTER).unwrap(), "60");
    }

    #[tokio::test]
    async fn a_wait_still_in_force_doubles_the_next_one() {
        let now = Arc::new(Mutex::new(1_700_000_000u64));
        let now_clone = now.clone();
        let (settings, _dir) = {
            let dir = tempfile::tempdir().unwrap();
            let settings = Arc::new(SettingsServer::new(
                dir.path().to_path_buf(),
                4700,
                |_: &str| None,
                move || *now_clone.lock().unwrap(),
                |_: i32| {},
            ));
            (settings, dir)
        };
        let app = app(settings.clone());

        for _ in 0..5 {
            app.clone()
                .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
                .await
                .unwrap();
        }
        let first_block = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(first_block.headers().get(header::RETRY_AFTER).unwrap(), "60");

        // Waiting out the first block and failing again, still without ever succeeding,
        // doubles the wait rather than starting back at a minute.
        *now.lock().unwrap() += 60;
        let sixth_failure = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(sixth_failure.status(), StatusCode::UNAUTHORIZED);
        let second_block = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(second_block.headers().get(header::RETRY_AFTER).unwrap(), "120");
    }

    #[tokio::test]
    async fn a_body_over_16kb_is_refused() {
        let (settings, _dir) = test_server();
        let big = format!(r#"{{"token":"{}"}}"#, "a".repeat(20 * 1024));
        let res = app(settings)
            .oneshot(post_body("/api/settings/session", &big))
            .await
            .unwrap();
        assert!(res.status().is_client_error());
    }
}
