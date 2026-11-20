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
pub mod docker_trust;
mod link;
mod programs;
mod secrets;
#[cfg(test)]
pub(crate) use auth::COOKIE_NAME;
use auth::InnerState;
pub(crate) use auth::{SESSION_TTL_SECS, csrf_ok, security_envelope};
pub use auth::{TOKEN_TTL_SECS, random_hex};
pub(crate) use programs::ProgramUpdate;
pub(crate) use secrets::secret_rows;

pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024;

/// Where a page's network access and password live: the code agent's in `machine.json` (or its environment), the CI
/// runner's in `ci/settings.json`. Read on every request, so a save that turns access off takes effect at once.
pub trait Access: Send + Sync {
    /// `None` when it can't be read, which is refused like "off" (fail closed).
    fn network_access(&self) -> Option<NetworkAccess>;

    /// Whether a password set while network access is off still signs in from this computer itself. Both programs
    /// say yes (the code agent since 2026-11-16): a computer with no tray has nothing else to sign in with once its
    /// one-time link is spent. Another computer still needs network access on.
    fn password_signs_in_locally(&self) -> bool {
        false
    }
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
    /// The links started from the page (`link.rs`), and the version a link's code request carries.
    pub(crate) program_links: link::Flows,
    version: OnceLock<String>,
    /// With `RETASKABLE_TRUST_DOCKER_HOST=1`, the container's default gateway, read once at start: a request from it
    /// is from this computer (`docker_trust.rs`). `None` otherwise.
    pub(crate) trusted_gateway: Option<std::net::IpAddr>,
    /// The first-visit door (`first_visit.rs`): whether a visit from the trusted gateway signs in with no token while
    /// the program says it is not set up yet. Unset: shut.
    first_visit: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
}

impl PageHost {
    pub fn new(
        dir: PathBuf,
        port: u16,
        env: Env,
        now: impl Fn() -> u64 + Send + Sync + 'static,
        access: impl Access + 'static,
    ) -> Self {
        let trusted_gateway = docker_trust::gateway(&env);
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
            program_links: link::Flows::default(),
            version: OnceLock::new(),
            trusted_gateway,
            first_visit: OnceLock::new(),
        }
    }

    /// Opens the first-visit door while `open` says the program still has setup to do (docs/specs/setup-wizard.md
    /// section 3.1). The door takes only a visit from the trusted Docker gateway: a native install has none.
    pub fn set_first_visit_door(&self, open: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = self.first_visit.set(Box::new(open));
    }

    /// Whether a sign-in with no token is let in from `from` right now.
    pub(crate) fn first_visit_open(&self, from: std::net::IpAddr) -> bool {
        self.trusted_gateway == Some(from.to_canonical()) && self.first_visit.get().is_some_and(|open| open())
    }
}

/// The 404 for a path this port doesn't serve. A request the Host/Origin checks refuse gets the same one, so it gives
/// away nothing about which paths exist.
pub fn not_found_response() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response()
}

/// The shared look, the helpers (`window.RT`) and the programs panel's script, which every page is built from.
static BASE_CSS: &str = include_str!("../../assets/settings-base.css");
static RT_JS: &str = include_str!("../../assets/settings-rt.js");
static SECRETS_JS: &str = include_str!("../../assets/settings-secrets.js");
static PROGRAMS_JS: &str = include_str!("../../assets/settings-programs.js");
static PROJECTS_JS: &str = include_str!("../../assets/settings-projects.js");
static WIZARD_JS: &str = include_str!("../../assets/settings-wizard.js");

/// A page's template with the shared parts put in: `{{BASE_CSS}}` inside its style, `{{RT_JS}}`, `{{SECRETS_JS}}`
/// (the secrets editor) `{{PROJECTS_JS}}` (the code agent's Add project) and `{{WIZARD_JS}}` (the setup wizard) in one script, and `{{PROGRAMS_JS}}` in a script of its own. `{{NONCE}}` is left for each request.
pub fn assemble(template: &str) -> Arc<str> {
    template
        .replace("{{BASE_CSS}}", BASE_CSS)
        .replace("{{RT_JS}}", RT_JS)
        .replace("{{SECRETS_JS}}", SECRETS_JS)
        .replace("{{PROGRAMS_JS}}", PROGRAMS_JS)
        .replace("{{PROJECTS_JS}}", PROJECTS_JS)
        .replace("{{WIZARD_JS}}", WIZARD_JS)
        .into()
}

/// The routes every page has, under the Host/Origin envelope and the body limit: `GET /settings` (`html`, with
/// `{{NONCE}}` where the page's nonce goes), `POST /api/settings/session`, and the programs panel's two.
pub fn routes(host: Arc<PageHost>, html: Arc<str>) -> Router {
    Router::new()
        .route("/settings", get(move || serve_page(html.clone())))
        .route("/api/settings/session", post(post_session))
        .route("/api/settings/programs", get(programs::get_programs))
        .route("/api/settings/programs/{program}", post(programs::post_program))
        .route("/api/settings/programs/{program}/link", post(link::post_program_link))
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
    /// The page's first visit, with no token: let in only through the door (`first_visit_open`).
    #[serde(rename = "firstVisit")]
    first_visit: bool,
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
    // The door shut is not a wrong guess, so it counts against nobody (a real sign-in right after still works).
    if req.first_visit && req.token.is_none() && req.password.is_none() && !host.first_visit_open(addr) {
        return unauthorized("Sign in with the link in the log or the password.");
    }
    let signed_in = match (&req.token, &req.password) {
        (Some(token), _) => host.verify_token(token, addr),
        (None, Some(password)) => host.verify_password(password, addr),
        (None, None) => req.first_visit,
    };
    if !signed_in {
        host.record_failure(addr);
        return unauthorized("Wrong or expired token or password.");
    }
    host.record_success(addr);
    let (cookie, csrf) = host.create_session();
    let set_cookie = format!(
        "{}={cookie}; HttpOnly; SameSite=Strict; Max-Age={SESSION_TTL_SECS}; Path=/",
        host.cookie_name()
    );
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
