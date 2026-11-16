//! What a settings page needs to be served and signed into, whichever program serves it (agent-settings.md section 4,
//! programs.md): sessions, the one-time token and the network password, the CSRF check, the wrong-try rate limit, the
//! Host and Origin checks every request goes through, the 16 KB body limit, the page itself, and the programs panel's
//! two routes (`programs.rs`). The code agent's `settings_server` and the CI runner's `agent_ci::serve` both build on
//! this; it knows no program's internals. What differs is only who answers "is network access on, with which
//! password" (`Access`) and which page is served.
//!
//! A neutral module (the boundary test, `program_boundaries_tests.rs`): it names neither the orchestrator nor the
//! chat or CI modules, and the routes it carries are `routes_tests.rs`'s allowlist.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::env::Env;
use crate::link::TokenStore;
use crate::network::NetworkAccess;

mod auth;
mod programs;
mod secrets;
use auth::InnerState;
pub(crate) use auth::{COOKIE_NAME, SESSION_TTL_SECS, csrf_ok, security_envelope};
pub use auth::{TOKEN_TTL_SECS, random_hex};
#[cfg(test)]
pub(crate) use programs::ProgramUpdate;
pub(crate) use secrets::secret_rows;

pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024;

/// Where a page's network access and password live: the code agent's in `machine.json` (or its environment), the CI
/// runner's in `ci/settings.json`. Read on every request, so a save that turns access off takes effect at once.
pub trait Access: Send + Sync {
    /// `None` when it can't be read, which is refused like "off" (fail closed).
    fn network_access(&self) -> Option<NetworkAccess>;
}

/// One page's server-side state: one instance shared (behind an `Arc`) by every request.
pub struct PageHost {
    /// The data folder; each program keeps its own files in a subfolder of it (`paths`).
    pub(crate) dir: PathBuf,
    /// The port the page is actually answering on, to check a request's Host (section 4.3).
    pub(crate) port: u16,
    /// This process's environment: the key of saved secrets and `RETASKABLE_SETTINGS_PUBLISHED_LOCAL`.
    pub(crate) env: Env,
    /// This computer's own interface addresses, for a Host that is one of them (section 4.3).
    pub(crate) own_addresses: Box<dyn Fn() -> Vec<std::net::IpAddr> + Send + Sync>,
    pub(crate) now: Box<dyn Fn() -> u64 + Send + Sync>,
    access: Box<dyn Access>,
    pub(crate) state: Mutex<InnerState>,
    /// The OS credential store for the data key, for the programs' secrets (`programs.rs`); none until given.
    program_keys: OnceLock<Option<Arc<dyn TokenStore>>>,
}

impl PageHost {
    pub fn new(
        dir: PathBuf,
        port: u16,
        env: Env,
        now: impl Fn() -> u64 + Send + Sync + 'static,
        access: impl Access + 'static,
    ) -> Self {
        Self {
            dir,
            port,
            env,
            own_addresses: Box::new(|| {
                if_addrs::get_if_addrs()
                    .map(|ifs| ifs.into_iter().map(|i| i.ip()).collect())
                    .unwrap_or_default()
            }),
            now: Box::new(now),
            access: Box::new(access),
            state: Mutex::new(InnerState::default()),
            program_keys: OnceLock::new(),
        }
    }
}

/// The 404 for a path this port doesn't serve. A request the Host/Origin checks refuse gets the same one, so it gives
/// away nothing about which paths exist.
pub fn not_found_response() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response()
}

/// The routes every page has, under the Host/Origin envelope and the body limit: `GET /settings` (`html`, with
/// `{{NONCE}}` where the page's nonce goes), `POST /api/settings/session`, and the programs panel's two.
pub fn routes(host: Arc<PageHost>, html: Arc<str>) -> Router {
    Router::new()
        .route("/settings", get(move || serve_page(html.clone())))
        .route("/api/settings/session", post(post_session))
        .route("/api/settings/programs", get(programs::get_programs))
        .route("/api/settings/programs/{program}", post(programs::post_program))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(host.clone(), security_envelope))
        .with_state(host)
}

async fn serve_page(html: Arc<str>) -> Response {
    let nonce = random_hex(16);
    let html = html.replace("{{NONCE}}", &nonce);
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
    State(host): State<Arc<PageHost>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: axum::body::Bytes,
) -> Response {
    let addr = addr.ip();
    if let Some(wait_secs) = host.check_rate_limit(addr) {
        return too_many_requests(wait_secs);
    }
    let Ok(req) = serde_json::from_slice::<SessionRequest>(&body) else {
        host.record_failure(addr);
        return bad_request("No JSON body.");
    };
    let signed_in = match (&req.token, &req.password) {
        (Some(token), _) => host.verify_token(token, addr),
        (None, Some(password)) => host.verify_password(password),
        (None, None) => false,
    };
    if !signed_in {
        host.record_failure(addr);
        return unauthorized("Wrong or expired token or password.");
    }
    host.record_success(addr);
    let (cookie, csrf) = host.create_session();
    let set_cookie = format!("{COOKIE_NAME}={cookie}; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL_SECS}; Path=/");
    ([(header::SET_COOKIE, set_cookie)], Json(json!({ "csrfToken": csrf }))).into_response()
}

impl PageHost {
    /// Gives the page the OS credential store the data key of saved secrets lives in. Without one (and without
    /// `RETASKABLE_SECRETS_KEY_FILE` or `RETASKABLE_SECRETS_KEY`) another program's secrets can't be saved here.
    pub fn set_program_keys(&self, store: Option<Arc<dyn TokenStore>>) {
        let _ = self.program_keys.set(store);
    }
}

pub(crate) fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub(crate) fn bad_request(message: &str) -> Response {
    json_error(StatusCode::BAD_REQUEST, message)
}

pub(crate) fn unauthorized(message: &str) -> Response {
    json_error(StatusCode::UNAUTHORIZED, message)
}

pub(crate) fn internal_error(message: &str) -> Response {
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

/// An `Access` with a fixed answer, for a test that doesn't care where network access is kept.
#[cfg(test)]
pub(crate) struct FixedAccess(pub(crate) Option<NetworkAccess>);

#[cfg(test)]
impl Access for FixedAccess {
    fn network_access(&self) -> Option<NetworkAccess> {
        self.0.clone()
    }
}
