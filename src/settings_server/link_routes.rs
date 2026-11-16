//! The **Link** buttons of the code agent's tab (agent-settings.md section 4.6): `POST /api/link/{target}` starts this
//! agent's own device flow for `production`, `staging`, `local`, a custom cloud, or `backlog` (the shared backlog store),
//! in the running agent (`link::pending::Links`, the tray's), and `GET /api/link` shows what it waits on. A link to
//! retaskable.com goes on to the store's, as `retaskable link` does, so one click leads to a computer that works. Like a
//! saved setting it is for this computer only and behind the session, CSRF, Host and Origin checks; nothing else runs.
//! The page opens the approval address as a link, so the browser isn't opened for it a second time.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::SettingsServer;
use crate::backlog;
use crate::config;
use crate::link::pending::StartOpts;
use crate::link::{self, BACKLOG_TARGET, CLOUD_TARGET};
use crate::page_host::{csrf_ok, internal_error, json_error, not_found_response, unauthorized};

pub(super) async fn post_link(
    State(settings): State<Arc<SettingsServer>>,
    UrlPath(target): UrlPath<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let Some(session) = settings.authenticate(&headers) else {
        return unauthorized("Sign in first.");
    };
    if !csrf_ok(&headers, &session.csrf) {
        return unauthorized("Missing or wrong CSRF token.");
    }
    if !settings.is_this_computer(peer.ip(), &headers) {
        return json_error(StatusCode::FORBIDDEN, "Linking is done only on this computer.");
    }
    let Some(links) = settings.links.get() else {
        return internal_error("This agent can't link from the page.");
    };
    let cfg = match config::load(&settings.dir) {
        Ok(cfg) => cfg,
        Err(e) => return internal_error(&e.to_string()),
    };
    let store_url = backlog::url(&settings.env);
    let (url, goal) = if target == "backlog" {
        (store_url.clone(), BACKLOG_TARGET)
    } else {
        let found = link::cloud_urls(&cfg, &settings.env, None)
            .into_iter()
            .find(|u| link::environment_name(u).eq_ignore_ascii_case(&target));
        match found {
            Some(url) => (url, CLOUD_TARGET),
            None => return not_found_response(),
        }
    };
    if let Err(e) = link::valid_http_url(&url, goal.name) {
        return json_error(StatusCode::BAD_REQUEST, &format!("{e:#}"));
    }
    if matches!(settings.keychain.get(&url), Ok(Some(_))) {
        return json_error(StatusCode::CONFLICT, "Already linked.");
    }
    if goal.device_path == BACKLOG_TARGET.device_path {
        backlog::clear_unlinked_on_site(); // the person's own "Link again"
    }
    let opts = StartOpts {
        open_browser: false,
        then_backlog: (goal.device_path == CLOUD_TARGET.device_path).then_some(store_url),
    };
    if links.start_flow(&url, goal, opts) {
        Json(json!({ "linking": true })).into_response()
    } else {
        json_error(StatusCode::CONFLICT, "A link is already waiting for approval.")
    }
}
