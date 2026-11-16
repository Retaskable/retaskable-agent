//! The computer-only settings page (agent-settings.md section 4, agent/08-http-api.md): `GET
//! /settings`, `POST /api/settings/session`, `GET`/`POST /api/settings`, `POST
//! /api/settings/restart`. Every request's Host must be this computer's own, a POST's Origin its
//! own, and a change the session's CSRF token (section 4.3); signing in is by a one-time tray
//! token, or, once network access is on, this computer's password (section 4.2). Sessions, sign-in, those checks and
//! the programs panel's routes are `page_host`'s, shared with the CI runner's page; what is here is the code agent's.
use crate::env::Env;
use std::net::SocketAddr;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::HeaderMap;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::config;
use crate::hello_settings::{HelloSettings, env_vars_json};
use crate::link::{self, TokenStore};
use crate::network::{self, NetworkAccess};
use crate::page_host::{self, Access, PageHost, bad_request, csrf_ok, internal_error, unauthorized};
use crate::profile;
use crate::store::MachineStore;
use crate::tags;

mod first_run;
mod link_routes;
mod project_routes;
pub(crate) mod save;
mod secrets;
mod status;
pub(crate) use crate::page_host::not_found_response;
pub(crate) use save::engine_choice_json;
use save::{SettingsUpdate, save_error_response};
pub use status::StatusSource;

/// agent-settings.md section 3.2: the environment variables the settings page greys out.
pub(crate) const AGENT_ENV_VARS: [&str; 8] = [
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
    /// Sessions, sign-in and the programs panel, shared with the CI runner's page (`page_host`).
    host: Arc<PageHost>,
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
    exit: Box<dyn Fn(i32) + Send + Sync>,
    /// This machine's stored cloud tokens, for `GET /api/link` (section 9.2's "computer's link
    /// state", local only).
    keychain: Arc<dyn TokenStore>,
    /// The Now tab's live inputs (`status.rs`), given once at start.
    status: OnceLock<StatusSource>,
    /// Fired after a save, so every project's connection sends a fresh hello (given once at start).
    saved: OnceLock<HelloSettings>,
    /// The running agent's device-link state, for `GET /api/backlog/link`'s `waiting` (given once at start).
    links: OnceLock<link::pending::Links>,
    /// The tokens its tools use, for `secrets` in `GET`/`POST /api/settings` (given once at start).
    secrets: OnceLock<Arc<crate::secrets::Secrets>>,
    /// **Add project** (`add_project`), given once at start.
    add_project: OnceLock<Arc<crate::add_project::AddProject>>,
}

/// The code agent's network access: `RETASKABLE_NETWORK_PASSWORD`'s, else `machine.json`'s, read on each call so a
/// save that turns it off takes effect at once (section 4.4). Unreadable is `None` (fail closed).
struct MachineAccess {
    dir: PathBuf,
    env: Option<NetworkAccess>,
}

impl Access for MachineAccess {
    fn network_access(&self) -> Option<NetworkAccess> {
        match &self.env {
            Some(access) => Some(access.clone()),
            None => Some(MachineStore::load(&self.dir).ok()?.network().unwrap_or_default()),
        }
    }
}

/// The page's own state is `page_host`'s; its fields (`dir`, `port`, `env`, `now`) read through this.
impl Deref for SettingsServer {
    type Target = PageHost;
    fn deref(&self) -> &PageHost {
        &self.host
    }
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
        let access = MachineAccess {
            dir: dir.clone(),
            env: env_network_access.clone(),
        };
        Self {
            host: Arc::new(PageHost::new(dir, port, env, now, access)),
            default_port,
            network_enabled,
            env_network_access,
            exit: Box::new(exit),
            keychain,
            status: OnceLock::new(),
            saved: OnceLock::new(),
            links: OnceLock::new(),
            secrets: OnceLock::new(),
            add_project: OnceLock::new(),
        }
    }

    /// The address that opens the settings page signed in (agent-settings.md section 4.2).
    pub fn settings_link(&self) -> String {
        self.host.settings_link()
    }

    /// The OS credential store for the data key, for the other programs' secrets (agent-settings.md section 4.6).
    pub fn set_program_keys(&self, store: Option<Arc<dyn TokenStore>>) {
        self.host.set_program_keys(store);
    }

    fn saver(&self) -> save::Saver<'_> {
        save::Saver {
            dir: &self.dir,
            env: &self.env,
        }
    }

    /// Gives the page its Add project flow; without one `/api/projects` answers that it can't.
    pub fn set_add_project(&self, add: Arc<crate::add_project::AddProject>) {
        let _ = self.add_project.set(add);
    }

    /// The signal a successful save fires.
    pub fn set_saved_signal(&self, signal: HelloSettings) {
        let _ = self.saved.set(signal);
    }

    /// Gives `GET /api/backlog/link` the running agent's device-link state; with none, `waiting` is null.
    pub fn set_links(&self, links: link::pending::Links) {
        let _ = self.links.set(links);
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
        let mut settings = json!({
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
            "envVars": env_vars_json(env),
        });
        settings["ciSlots"] = json!(config::ci_slots(&cfg, env));
        settings["cursorAllow"] = Self::cursor_allow_json(&cfg, env);
        Ok(settings)
    }

    /// Each project's `engines.allow`, the programs derived from its checks, and the
    /// allowlist file Cursor follows there (engines/06-permissions-the-allowlist.md section 6.4).
    fn cursor_allow_json(cfg: &config::ConfigFile, env: &Env) -> Value {
        let rows = cfg.projects.iter().map(|p| {
            let (line, problem) = crate::engines::cursor::allowlist_in_force(std::path::Path::new(&p.repo_path), env);
            let allow = p
                .engines
                .as_ref()
                .and_then(|e| e.get("allow"))
                .cloned()
                .unwrap_or(json!([]));
            json!({
                "project": p.id_or_derived(),
                "allow": allow,
                "commandShells": crate::engines::cursor::command_shells(&p.checks),
                "inForce": { "line": line, "problem": problem },
            })
        });
        Value::Array(rows.collect())
    }

    /// `GET /api/link`'s view of each of retaskable.com's three environments, and a custom
    /// `cloud.url` when one is set (tray.md section 4.2's "Link to" submenu), with whether this
    /// machine already holds a stored token for it, and of the shared backlog store. While a link started from this page
    /// waits, `waiting` is the address that approves it and its code; a link that ended badly says why in `error`.
    /// `canLink` is whether this request may start one (only from this computer). Never a token.
    fn link_view(&self, can_link: bool) -> Result<Value> {
        let env = &self.env;
        let cfg = config::load(&self.dir)?;
        let row = |name: &str, url: &str| {
            let connected = matches!(self.keychain.get(url), Ok(Some(_)));
            let mut row = json!({ "name": name, "url": url, "connected": connected, "waiting": null, "error": null });
            if let Some(links) = self.links.get() {
                row["working"] = json!(!links.start_allowed(url));
                row["waiting"] = links.waiting(url).map_or(
                    Value::Null,
                    |w| json!({ "url": link::pending::web_address(&w.page_url), "code": w.user_code }),
                );
                if let (false, Some(link::pending::Outcome::Failed(why))) = (connected, links.outcome(url)) {
                    row["error"] = json!(why);
                }
            }
            row
        };
        let environments: Vec<Value> = link::cloud_urls(&cfg, env, None)
            .into_iter()
            .map(|url| row(link::environment_name(&url), &url))
            .collect();
        Ok(json!({
            "environments": environments,
            "backlog": row("Shared backlog", &crate::backlog::url(env)),
            "canLink": can_link,
        }))
    }

    /// `GET /api/backlog/link`: whether this machine holds a token for the shared backlog's store, the
    /// store's address, and the approval page while a device-link code waits.
    fn backlog_link_view(&self) -> Value {
        let url = crate::backlog::url(&self.env);
        let linked = matches!(self.keychain.get(&url), Ok(Some(_)));
        let waiting = self
            .links
            .get()
            .and_then(|l| l.backlog())
            .map(|w| json!({ "url": w.page_url, "label": w.label }));
        json!({ "linked": linked, "url": url, "waiting": waiting })
    }
}

/// The settings page's own routes (agent/08-http-api.md), merged into the agent's router
/// alongside `/api/health`.
pub fn router(settings: Arc<SettingsServer>) -> Router {
    let html = page_host::assemble(SETTINGS_HTML);
    Router::new()
        .route("/api/settings", get(get_settings).post(post_settings))
        .route("/api/settings/status", get(status::get_status))
        .route("/api/settings/restart", post(post_restart))
        .route("/api/link", get(get_link))
        .route("/api/link/{target}", post(link_routes::post_link))
        .route("/api/backlog/link", get(get_backlog_link))
        .route("/api/profile", get(get_profile))
        .route(
            "/api/projects",
            get(project_routes::get_projects).post(project_routes::post_project),
        )
        .route("/api/projects/{id}/paused", post(project_routes::post_paused))
        .route("/api/projects/owners", get(project_routes::get_owners))
        .route("/api/projects/repos", get(project_routes::get_repos))
        .layer(DefaultBodyLimit::max(page_host::MAX_BODY_BYTES))
        .route_layer(middleware::from_fn_with_state(
            settings.host.clone(),
            page_host::security_envelope,
        ))
        .with_state(settings.clone())
        .merge(page_host::routes(settings.host.clone(), html))
}

async fn get_settings(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    match settings.current_settings() {
        Ok(mut value) => {
            settings.add_secrets(&mut value, peer.ip(), &headers);
            Json(value).into_response()
        }
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn post_settings(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
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
    // Secrets are checked with everything else before any of it is written.
    let plan = match settings.plan_secrets(update.secrets.as_ref(), peer.ip(), &headers) {
        Ok(plan) => plan,
        Err(refused) => return *refused,
    };
    match settings.saver().apply(&update) {
        Ok(()) => {
            if let Err(e) = settings.commit_secrets(plan) {
                return *e;
            }
            if let Some(signal) = settings.saved.get() {
                signal.fire();
            }
            Json(json!({ "saved": true })).into_response()
        }
        Err(e) => save_error_response(e),
    }
}

async fn get_link(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    match settings.link_view(settings.is_this_computer(peer.ip(), &headers)) {
        Ok(value) => Json(value).into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

async fn get_backlog_link(State(settings): State<Arc<SettingsServer>>, headers: HeaderMap) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    Json(settings.backlog_link_view()).into_response()
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

/// What the tests in this module's children name through `super::*`.
#[cfg(test)]
use {
    crate::page_host::{COOKIE_NAME, SESSION_TTL_SECS, random_hex},
    axum::http::{StatusCode, header},
    std::sync::Mutex,
};

#[cfg(test)]
mod link_tests;
#[cfg(test)]
mod network_tests;
#[cfg(test)]
mod program_link_tests;
#[cfg(test)]
mod programs_tests;
#[cfg(test)]
mod project_tests;
#[cfg(test)]
mod routes_tests;
#[cfg(test)]
mod secrets_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
