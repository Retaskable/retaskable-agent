//! The settings page on the agent's port (agent-settings.md section 4; TypeScript's
//! `SettingsSite` in `settingsServer.ts`): `GET /settings`, `GET`/`POST /api/settings`,
//! `POST /api/settings/session`, `POST /api/settings/restart`. Only the computer's own
//! settings, never a command. Signing in takes the tray's one-time token, or, with network
//! access on, the password. Every request's Host must be the agent's own address (no DNS
//! rebinding), every POST's Origin its own and its CSRF token the session's.
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as base64_std, URL_SAFE_NO_PAD as base64_url};
use rand::RngExt;
use serde_json::{Value, json};

use super::core::{self, SettingsChange};
use crate::{config, network, password, store};

/// A tray link's token works once, within this long.
const TOKEN_MS: u64 = 5 * 60_000;
/// A signed-in session lasts this long.
const SESSION_MS: u64 = 8 * 60 * 60_000;
/// Wrong tokens or passwords from one address: this many a minute, then it waits, twice as long
/// each time.
const MAX_WRONG: usize = 5;
const WRONG_WINDOW_MS: u64 = 60_000;
const MAX_LOCK_MS: u64 = 60 * 60_000;
const COOKIE_NAME: &str = "retaskable_settings";
const MAX_BODY: usize = 16 * 1024;

const SAFE_HEADERS: [(&str, &str); 4] = [
    ("cache-control", "no-store"),
    ("x-frame-options", "DENY"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
];

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    base64_url.encode(bytes)
}

/// A tray link's one-time tokens, each good once within [`TOKEN_MS`].
#[derive(Debug, Default)]
struct TokenStore {
    tokens: HashMap<String, u64>,
}

impl TokenStore {
    fn mint(&mut self, now: u64) -> String {
        self.tokens.retain(|_, until| *until >= now);
        let token = random_token();
        self.tokens.insert(token.clone(), now + TOKEN_MS);
        token
    }

    /// Consumes `token`: good exactly once, and only within its window.
    fn consume(&mut self, token: &str, now: u64) -> bool {
        self.tokens.remove(token).is_some_and(|until| until >= now)
    }
}

/// Signed-in sessions, each with its own CSRF token, lasting [`SESSION_MS`].
#[derive(Debug, Default)]
struct SessionStore {
    sessions: HashMap<String, (String, u64)>,
}

impl SessionStore {
    fn create(&mut self, now: u64) -> (String, String) {
        let id = random_token();
        let csrf = random_token();
        self.sessions.insert(id.clone(), (csrf.clone(), now + SESSION_MS));
        (id, csrf)
    }

    fn csrf_for(&mut self, id: &str, now: u64) -> Option<String> {
        match self.sessions.get(id) {
            Some((csrf, until)) if *until >= now => Some(csrf.clone()),
            Some(_) => {
                self.sessions.remove(id);
                None
            }
            None => None,
        }
    }
}

/// Wrong sign-in tries from one address: [`MAX_WRONG`] within [`WRONG_WINDOW_MS`] locks it out,
/// doubling the wait each time, up to [`MAX_LOCK_MS`].
#[derive(Debug, Default)]
struct RateLimiter {
    wrong: HashMap<String, Wrong>,
}

#[derive(Debug, Default)]
struct Wrong {
    at: Vec<u64>,
    locked_until: u64,
    locks: u32,
}

impl RateLimiter {
    fn locked_for(&self, ip: &str, now: u64) -> u64 {
        self.wrong
            .get(ip)
            .map(|w| w.locked_until.saturating_sub(now))
            .unwrap_or(0)
    }

    fn count_wrong(&mut self, ip: &str, now: u64) {
        let w = self.wrong.entry(ip.to_string()).or_default();
        w.at.retain(|&t| now.saturating_sub(t) < WRONG_WINDOW_MS);
        w.at.push(now);
        if w.at.len() >= MAX_WRONG {
            let factor = 1u64.checked_shl(w.locks).unwrap_or(u64::MAX);
            w.locked_until = now + WRONG_WINDOW_MS.saturating_mul(factor).min(MAX_LOCK_MS);
            w.locks += 1;
            w.at.clear();
        }
    }

    fn clear(&mut self, ip: &str) {
        self.wrong.remove(ip);
    }
}

/// `address === "::1" || address.startsWith("127.") || address.startsWith("::ffff:127.")`.
fn is_loopback(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

/// This computer's own addresses on its networks, as a Host header names them
/// ("192.168.1.20", "[fe80::1]").
fn lan_addresses() -> Vec<String> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| match i.ip() {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        })
        .collect()
}

/// The Host is this computer's own address at the port it answered on: 127.0.0.1, localhost or
/// [::1], and its network addresses while network access is on. Anything else (a DNS name
/// pointed here) is refused.
fn host_ok(host: Option<&str>, port: u16, network_enabled: bool, lan: &[String]) -> bool {
    let Some(host) = host else { return false };
    let mut names = vec!["127.0.0.1".to_string(), "localhost".to_string(), "[::1]".to_string()];
    if network_enabled {
        names.extend(lan.iter().cloned());
    }
    names.iter().any(|n| host == format!("{n}:{port}"))
}

fn origin_ok(origin: Option<&str>, host: Option<&str>) -> bool {
    matches!((origin, host), (Some(o), Some(h)) if o == format!("http://{h}"))
}

fn csrf_ok(given: Option<&str>, want: &str) -> bool {
    given.is_some_and(|g| password::constant_time_eq(g.as_bytes(), want.as_bytes()))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn parse_body(headers: &HeaderMap, body: &Bytes) -> Result<Value, &'static str> {
    let ct = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !ct.to_ascii_lowercase().starts_with("application/json") {
        return Err("Send JSON.");
    }
    if body.len() > MAX_BODY {
        return Err("Too big.");
    }
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).map_err(|_| "Send JSON.")
}

fn send(status: StatusCode, body: Value) -> Response {
    let mut res = (status, Json(body)).into_response();
    for (k, v) in SAFE_HEADERS {
        res.headers_mut()
            .insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    res
}

struct Inner {
    dir: PathBuf,
    env: fn(&str) -> Option<String>,
    found_tags: Vec<String>,
    running_port: u16,
    running_network_enabled: bool,
    shutdown: Arc<tokio::sync::Notify>,
    tokens: Mutex<TokenStore>,
    sessions: Mutex<SessionStore>,
    rate_limit: Mutex<RateLimiter>,
    page: String,
}

/// Shared state for the settings page's routes: cheap to clone (an `Arc` inside), and safe to
/// share across requests.
#[derive(Clone)]
pub struct SettingsState {
    inner: Arc<Inner>,
}

impl SettingsState {
    pub fn new(
        dir: PathBuf,
        env: fn(&str) -> Option<String>,
        found_tags: Vec<String>,
        running_port: u16,
        running_network_enabled: bool,
        shutdown: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                dir,
                env,
                found_tags,
                running_port,
                running_network_enabled,
                shutdown,
                tokens: Mutex::new(TokenStore::default()),
                sessions: Mutex::new(SessionStore::default()),
                rate_limit: Mutex::new(RateLimiter::default()),
                page: include_str!("settings.html").to_string(),
            }),
        }
    }

    /// A one-time token for the tray's link (`/settings#<token>`), good once within
    /// [`TOKEN_MS`]. Nothing mints one yet: there's no tray in this agent.
    pub fn mint_token(&self) -> String {
        self.inner.tokens.lock().unwrap().mint(now_ms())
    }
}

pub fn router(state: SettingsState) -> Router {
    Router::new()
        .route("/settings", get(handle))
        .route("/api/settings", get(handle).post(handle))
        .route("/api/settings/session", post(handle))
        .route("/api/settings/restart", post(handle))
        .with_state(state)
}

async fn handle(
    State(state): State<SettingsState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match try_handle(&state, addr, &method, uri.path(), &headers, &body) {
        Ok(res) => res,
        Err(e) => send(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": e.to_string() })),
    }
}

struct Ctx {
    local: bool,
    network_enabled: bool,
}

fn network_enabled_now(state: &SettingsState) -> Result<bool> {
    Ok(network::network_access(
        store::MachineStore::load(&state.inner.dir)?.network().as_ref(),
        state.inner.env,
    )
    .enabled)
}

fn precheck(
    state: &SettingsState,
    addr: SocketAddr,
    headers: &HeaderMap,
    is_post: bool,
) -> Result<Result<Ctx, Response>> {
    let local = is_loopback(addr.ip());
    let network_enabled = network_enabled_now(state)?;
    if !local && !network_enabled {
        return Ok(Err(send(
            StatusCode::FORBIDDEN,
            json!({ "error": "Network access to this page is off." }),
        )));
    }
    let host = headers.get(axum::http::header::HOST).and_then(|v| v.to_str().ok());
    let lan = if network_enabled { lan_addresses() } else { Vec::new() };
    if !host_ok(host, state.inner.running_port, network_enabled, &lan) {
        return Ok(Err(send(
            StatusCode::FORBIDDEN,
            json!({ "error": "Open this page at 127.0.0.1." }),
        )));
    }
    if is_post {
        let origin = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok());
        if !origin_ok(origin, host) {
            return Ok(Err(send(
                StatusCode::FORBIDDEN,
                json!({ "error": "Refused: not from this page." }),
            )));
        }
    }
    Ok(Ok(Ctx { local, network_enabled }))
}

fn session_csrf(state: &SettingsState, headers: &HeaderMap) -> Option<String> {
    let id = cookie_value(headers, COOKIE_NAME)?;
    state.inner.sessions.lock().unwrap().csrf_for(&id, now_ms())
}

fn try_handle(
    state: &SettingsState,
    addr: SocketAddr,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<Response> {
    let is_post = *method == Method::POST;
    let ctx = match precheck(state, addr, headers, is_post)? {
        Err(res) => return Ok(res),
        Ok(ctx) => ctx,
    };
    if path == "/settings" {
        return Ok(send_page(state));
    }
    if path == "/api/settings/session" {
        return sign_in(state, addr, ctx.local, ctx.network_enabled, headers, body);
    }
    let Some(csrf) = session_csrf(state, headers) else {
        return Ok(send(
            StatusCode::UNAUTHORIZED,
            json!({ "error": "Sign in first.", "signIn": if ctx.local { "tray" } else { "password" } }),
        ));
    };
    if *method == Method::GET {
        return Ok(send(StatusCode::OK, settings_body(state, csrf, false)?));
    }
    let given = headers.get("x-csrf-token").and_then(|v| v.to_str().ok());
    if !csrf_ok(given, &csrf) {
        return Ok(send(
            StatusCode::FORBIDDEN,
            json!({ "error": "Refused: reload the page and try again." }),
        ));
    }
    if path == "/api/settings/restart" {
        state.inner.shutdown.notify_waiters();
        return Ok(send(StatusCode::OK, json!({ "ok": true })));
    }
    let value = match parse_body(headers, body) {
        Ok(v) => v,
        Err(msg) => return Ok(send(StatusCode::BAD_REQUEST, json!({ "error": msg }))),
    };
    let change: SettingsChange = match serde_json::from_value(value) {
        Ok(c) => c,
        Err(_) => return Ok(send(StatusCode::BAD_REQUEST, json!({ "error": "Send JSON." }))),
    };
    if let Err(msg) = save_change(state, &change)? {
        return Ok(send(StatusCode::BAD_REQUEST, json!({ "error": msg })));
    }
    Ok(send(StatusCode::OK, settings_body(state, csrf, true)?))
}

fn send_page(state: &SettingsState) -> Response {
    let mut nonce_bytes = [0u8; 16];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = base64_std.encode(nonce_bytes);
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'"
    );
    let html = state.inner.page.replace("{{nonce}}", &nonce);
    let mut res = (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response();
    res.headers_mut().insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_str(&csp).expect("a base64 nonce is a valid header value"),
    );
    for (k, v) in SAFE_HEADERS {
        res.headers_mut()
            .insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
    res
}

/// `{ token }` from the tray's link (on this computer only), or `{ password }` with network
/// access on. Wrong ones count against the address, which waits once it has had too many.
fn sign_in(
    state: &SettingsState,
    addr: SocketAddr,
    local: bool,
    network_enabled: bool,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<Response> {
    let ip = addr.ip().to_string();
    let now = now_ms();
    let wait = state.inner.rate_limit.lock().unwrap().locked_for(&ip, now);
    if wait > 0 {
        return Ok(send(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": format!("Too many tries: wait {} seconds.", wait.div_ceil(1000)) }),
        ));
    }
    let value = parse_body(headers, body).unwrap_or(json!({}));
    let token = value.get("token").and_then(Value::as_str);
    let password_given = value.get("password").and_then(Value::as_str);
    let mut ok = false;
    if let Some(t) = token {
        if local {
            ok = state.inner.tokens.lock().unwrap().consume(t, now);
        }
    } else if let Some(p) = password_given
        && network_enabled
    {
        let stored = store::MachineStore::load(&state.inner.dir)?.network();
        ok = stored
            .and_then(|n| n.password_hash)
            .is_some_and(|h| password::verify_password(p, &h));
    }
    if !ok {
        state.inner.rate_limit.lock().unwrap().count_wrong(&ip, now);
        let msg = if password_given.is_some() {
            "Wrong password."
        } else {
            "This link has been used or is too old: open Settings… from the tray again."
        };
        return Ok(send(StatusCode::UNAUTHORIZED, json!({ "error": msg })));
    }
    state.inner.rate_limit.lock().unwrap().clear(&ip);
    let (id, csrf) = state.inner.sessions.lock().unwrap().create(now);
    let mut res = send(StatusCode::OK, json!({ "csrf": csrf }));
    let cookie = format!(
        "{COOKIE_NAME}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_MS / 1000
    );
    res.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("a base64 session id is a valid cookie value"),
    );
    Ok(res)
}

fn settings_body(state: &SettingsState, csrf: String, saved: bool) -> Result<Value> {
    let cfg = config::load(&state.inner.dir)?;
    let machine = store::MachineStore::load(&state.inner.dir)?;
    let (view, set_by) = core::current_settings(
        &cfg,
        machine.tags().as_ref(),
        machine.network().as_ref(),
        &state.inner.found_tags,
        state.inner.env,
    );
    let restart_needed =
        view.port != state.inner.running_port || view.network.enabled != state.inner.running_network_enabled;
    let mut body = json!({
        "settings": view,
        "setBy": set_by,
        "restartNeeded": restart_needed,
        "csrf": csrf,
    });
    if saved {
        body["saved"] = Value::Bool(true);
    }
    Ok(body)
}

fn save_change(state: &SettingsState, change: &SettingsChange) -> Result<Result<(), String>> {
    let cfg = config::load(&state.inner.dir)?;
    let machine = store::MachineStore::load(&state.inner.dir)?;
    let applied = match core::apply_settings(
        change,
        &cfg,
        machine.tags().as_ref(),
        machine.network().as_ref(),
        &state.inner.found_tags,
        state.inner.env,
    ) {
        Ok(a) => a,
        Err(msg) => return Ok(Err(msg)),
    };
    if !applied.config_changes.is_empty() {
        let mut store = config::save::ConfigStore::load(&state.inner.dir)?;
        for (key, value) in applied.config_changes {
            store.set(key, value);
        }
        store.save()?;
    }
    if let Some(tags) = applied.tags {
        store::MachineStore::load(&state.inner.dir)?.set_tags(tags)?;
    }
    if let Some(network) = applied.network {
        store::MachineStore::load(&state.inner.dir)?.set_network(network)?;
    }
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn state_at(port: u16, network_enabled: bool) -> SettingsState {
        let dir = tempfile::tempdir().unwrap();
        SettingsState::new(
            dir.keep(),
            no_env,
            vec!["windows".to_string()],
            port,
            network_enabled,
            Arc::new(tokio::sync::Notify::new()),
        )
    }

    #[test]
    fn loopback_addresses_are_recognised_including_v4_mapped_v6() {
        assert!(is_loopback("127.0.0.1".parse().unwrap()));
        assert!(is_loopback("127.5.5.5".parse().unwrap()));
        assert!(is_loopback("::1".parse().unwrap()));
        assert!(is_loopback("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_loopback("10.0.0.5".parse().unwrap()));
        assert!(!is_loopback("::ffff:10.0.0.5".parse().unwrap()));
    }

    #[test]
    fn host_ok_matches_loopback_names_at_the_running_port_only() {
        assert!(host_ok(Some("127.0.0.1:4700"), 4700, false, &[]));
        assert!(host_ok(Some("localhost:4700"), 4700, false, &[]));
        assert!(host_ok(Some("[::1]:4700"), 4700, false, &[]));
        assert!(!host_ok(Some("127.0.0.1:9999"), 4700, false, &[]));
        assert!(!host_ok(Some("evil.example:4700"), 4700, false, &[]));
        assert!(!host_ok(None, 4700, false, &[]));
    }

    #[test]
    fn host_ok_only_allows_lan_addresses_while_network_access_is_on() {
        let lan = vec!["192.168.1.20".to_string()];
        assert!(!host_ok(Some("192.168.1.20:4700"), 4700, false, &lan));
        assert!(host_ok(Some("192.168.1.20:4700"), 4700, true, &lan));
    }

    #[test]
    fn origin_ok_requires_the_exact_host_as_http() {
        assert!(origin_ok(Some("http://127.0.0.1:4700"), Some("127.0.0.1:4700")));
        assert!(!origin_ok(Some("https://127.0.0.1:4700"), Some("127.0.0.1:4700")));
        assert!(!origin_ok(Some("http://evil.example"), Some("127.0.0.1:4700")));
        assert!(!origin_ok(None, Some("127.0.0.1:4700")));
    }

    #[test]
    fn a_token_works_exactly_once_and_only_within_its_window() {
        let mut tokens = TokenStore::default();
        let token = tokens.mint(1_000);
        assert!(tokens.consume(&token, 1_000));
        assert!(!tokens.consume(&token, 1_000), "a token is good for one use");

        let token = tokens.mint(1_000);
        assert!(!tokens.consume(&token, 1_000 + TOKEN_MS + 1), "an expired token fails");
    }

    #[test]
    fn a_session_s_csrf_is_only_readable_within_its_window() {
        let mut sessions = SessionStore::default();
        let (id, csrf) = sessions.create(1_000);
        assert_eq!(sessions.csrf_for(&id, 1_000), Some(csrf));
        assert_eq!(sessions.csrf_for(&id, 1_000 + SESSION_MS + 1), None);
        assert_eq!(sessions.csrf_for("unknown", 1_000), None);
    }

    #[test]
    fn five_wrong_tries_in_a_minute_lock_the_address_out_doubling_each_time() {
        let mut rl = RateLimiter::default();
        for n in 0..4 {
            rl.count_wrong("1.2.3.4", 1_000 + n);
            assert_eq!(rl.locked_for("1.2.3.4", 1_000 + n), 0);
        }
        rl.count_wrong("1.2.3.4", 1_004);
        assert_eq!(rl.locked_for("1.2.3.4", 1_004), WRONG_WINDOW_MS);
        rl.clear("1.2.3.4");
        assert_eq!(rl.locked_for("1.2.3.4", 1_004), 0);
    }

    fn get(path: &str, addr: [u8; 4], extra: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method("GET").uri(path);
        for (k, v) in extra {
            builder = builder.header(*k, *v);
        }
        let mut req = builder.body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::from(addr), 5555))));
        req
    }

    fn post(path: &str, addr: [u8; 4], extra: &[(&str, &str)], body: Value) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri(path);
        for (k, v) in extra {
            builder = builder.header(*k, *v);
        }
        let mut req = builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::from(addr), 5555))));
        req
    }

    async fn body_json(res: Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    const HOST: &str = "127.0.0.1:4710";

    #[tokio::test]
    async fn the_page_serves_html_with_a_nonce_filled_in() {
        let app = router(state_at(4710, false));
        let req = get("/settings", [127, 0, 0, 1], &[("host", HOST)]);
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!html.contains("{{nonce}}"));
        assert!(html.contains("nonce=\""));
    }

    #[tokio::test]
    async fn a_wrong_host_is_refused_even_from_loopback() {
        let app = router(state_at(4710, false));
        let req = get("/settings", [127, 0, 0, 1], &[("host", "evil.example:4710")]);
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_non_loopback_address_is_refused_while_network_access_is_off() {
        let app = router(state_at(4710, false));
        let req = get("/settings", [10, 0, 0, 5], &[("host", HOST)]);
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            body_json(res).await["error"],
            Value::String("Network access to this page is off.".to_string())
        );
    }

    #[tokio::test]
    async fn api_settings_without_a_session_says_sign_in_with_the_tray() {
        let app = router(state_at(4710, false));
        let req = get("/api/settings", [127, 0, 0, 1], &[("host", HOST)]);
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(res).await["signIn"], Value::String("tray".to_string()));
    }

    #[tokio::test]
    async fn signing_in_with_a_wrong_token_fails_and_a_minted_one_works_once() {
        let state = state_at(4710, false);
        let app = router(state.clone());
        let req = post(
            "/api/settings/session",
            [127, 0, 0, 1],
            &[("host", HOST), ("origin", "http://127.0.0.1:4710")],
            json!({ "token": "not-a-real-token" }),
        );
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let token = state.mint_token();
        let req = post(
            "/api/settings/session",
            [127, 0, 0, 1],
            &[("host", HOST), ("origin", "http://127.0.0.1:4710")],
            json!({ "token": token }),
        );
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get("set-cookie").is_some());

        // The same token doesn't work a second time.
        let req = post(
            "/api/settings/session",
            [127, 0, 0, 1],
            &[("host", HOST), ("origin", "http://127.0.0.1:4710")],
            json!({ "token": token }),
        );
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_post_from_the_wrong_origin_is_refused() {
        let state = state_at(4710, false);
        let app = router(state.clone());
        let token = state.mint_token();
        let req = post(
            "/api/settings/session",
            [127, 0, 0, 1],
            &[("host", HOST), ("origin", "http://evil.example")],
            json!({ "token": token }),
        );
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn signing_in_then_saving_a_change_needs_the_csrf_token_and_a_restart() {
        let state = state_at(4710, false);
        let app = router(state.clone());
        let token = state.mint_token();
        let req = post(
            "/api/settings/session",
            [127, 0, 0, 1],
            &[("host", HOST), ("origin", "http://127.0.0.1:4710")],
            json!({ "token": token }),
        );
        let res = app.clone().oneshot(req).await.unwrap();
        let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
        let session_cookie = cookie.split(';').next().unwrap().to_string();
        let csrf = body_json(res).await["csrf"].as_str().unwrap().to_string();

        // Without the CSRF header, a save is refused.
        let req = post(
            "/api/settings",
            [127, 0, 0, 1],
            &[
                ("host", HOST),
                ("origin", "http://127.0.0.1:4710"),
                ("cookie", &session_cookie),
            ],
            json!({ "port": 4711 }),
        );
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);

        // With it, the change is checked, applied and reported as waiting for a restart.
        let mut req = post(
            "/api/settings",
            [127, 0, 0, 1],
            &[
                ("host", HOST),
                ("origin", "http://127.0.0.1:4710"),
                ("cookie", &session_cookie),
            ],
            json!({ "port": 4711 }),
        );
        req.headers_mut()
            .insert("x-csrf-token", HeaderValue::from_str(&csrf).unwrap());
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["saved"], Value::Bool(true));
        assert_eq!(body["restartNeeded"], Value::Bool(true));
        assert_eq!(body["settings"]["port"], Value::from(4711));
    }
}
