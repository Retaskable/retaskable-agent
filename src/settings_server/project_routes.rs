//! **Add project** on the code agent's tab (agent-settings.md section 4.7): the person's GitHub owners and repos to
//! pick from, the projects already added, and one add at a time. An add is a settings change (it writes `config.json`
//! and starts the project paused), not a command, so the POST has the same checks as a saved setting: the session, its
//! CSRF token, the Origin and Host (the envelope) and, like the Link buttons, this computer only. A GET needs only the
//! session. Nothing here names or carries a token: `add_project` scrubs every message it sends.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::SettingsServer;
use crate::add_project::{AddProject, Refusal};
use crate::page_host::{bad_request, csrf_ok, internal_error, json_error, unauthorized};

fn adder(settings: &SettingsServer) -> Result<Arc<AddProject>, Box<Response>> {
    settings
        .add_project
        .get()
        .cloned()
        .ok_or_else(|| Box::new(internal_error("This agent can't add a project from the page.")))
}

fn refused(r: Refusal) -> Response {
    match r {
        Refusal::Bad(m) => bad_request(&m),
        Refusal::Conflict(m) => json_error(StatusCode::CONFLICT, &m),
    }
}

/// `GET /api/projects`: the projects added, where the one add is, where a clone would go, and whether this request may add.
pub(super) async fn get_projects(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    let add = match adder(&settings) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let projects: Vec<Value> = add
        .projects()
        .into_iter()
        .map(|(id, repo)| json!({ "id": id, "githubRepo": repo }))
        .collect();
    Json(json!({
        "projects": projects,
        "add": add.status().json(),
        "cloneFolder": add.clone_folder("").to_string_lossy(),
        // Only this computer adds one; another computer signed in by password sees the list.
        "canAdd": settings.is_this_computer(peer.ip(), &headers),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct Listing {
    #[serde(default)]
    owner: String,
    #[serde(default)]
    fresh: Option<String>,
}

impl Listing {
    fn fresh(&self) -> bool {
        self.fresh.as_deref().is_some_and(|f| f == "1" || f == "true")
    }
}

/// `GET /api/projects/owners`: the person's GitHub login and organisations.
pub(super) async fn get_owners(
    State(settings): State<Arc<SettingsServer>>,
    headers: HeaderMap,
    Query(q): Query<Listing>,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    let add = match adder(&settings) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let fresh = q.fresh();
    match tokio::task::spawn_blocking(move || add.owners(fresh)).await {
        Ok(Ok(owners)) => Json(json!({ "owners": owners })).into_response(),
        Ok(Err(why)) => json_error(StatusCode::BAD_GATEWAY, &why),
        Err(_) => internal_error("Couldn't ask GitHub."),
    }
}

/// `GET /api/projects/repos?owner=`: that owner's repos, each with whether it is added already.
pub(super) async fn get_repos(
    State(settings): State<Arc<SettingsServer>>,
    headers: HeaderMap,
    Query(q): Query<Listing>,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    let add = match adder(&settings) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let fresh = q.fresh();
    match tokio::task::spawn_blocking(move || add.repos(&q.owner, fresh)).await {
        Ok(Ok(rows)) => {
            let repos: Vec<Value> = rows
                .into_iter()
                .map(|(r, added)| {
                    json!({
                        "nameWithOwner": r.name_with_owner,
                        "description": r.description,
                        "private": !r.visibility.eq_ignore_ascii_case("public"),
                        "fork": r.is_fork,
                        "added": added,
                    })
                })
                .collect();
            Json(json!({ "repos": repos })).into_response()
        }
        Ok(Err(Refusal::Conflict(why))) => json_error(StatusCode::BAD_GATEWAY, &why),
        Ok(Err(r)) => refused(r),
        Err(_) => internal_error("Couldn't ask GitHub."),
    }
}

#[derive(Deserialize)]
struct AddBody {
    repo: String,
}

/// `POST /api/projects` `{ repo }`: starts adding it. Answers at once; `GET /api/projects` says how it is going.
pub(super) async fn post_project(
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
    if !settings.is_this_computer(peer.ip(), &headers) {
        return json_error(StatusCode::FORBIDDEN, "Adding a project is done only on this computer.");
    }
    let add = match adder(&settings) {
        Ok(a) => a,
        Err(r) => return *r,
    };
    let Ok(AddBody { repo }) = serde_json::from_slice(&body) else {
        return bad_request("No JSON body with a repo.");
    };
    match add.start(&repo) {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({ "adding": true }))).into_response(),
        Err(r) => refused(r),
    }
}
