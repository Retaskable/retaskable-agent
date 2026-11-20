//! The wizard's routes. Each is behind the session; a change also has the CSRF token and is from this computer only,
//! as a saved secret or a Link button is (agent-settings.md section 4.3).
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::gather::CLAUDE_SECRETS;
use super::one_secret;
use super::steps::{self, Step};
use crate::page_host::{ProgramUpdate, bad_request, csrf_ok, internal_error, json_error, unauthorized};
use crate::secrets::fingerprint;
use crate::settings_server::SettingsServer;

type Fail = Box<Response>;

/// The checks every change has: the session, the CSRF token, and that it is from this computer.
fn may_change(settings: &SettingsServer, peer: &SocketAddr, headers: &HeaderMap) -> Result<(), Fail> {
    let Some(session) = settings.authenticate(headers) else {
        return Err(Box::new(unauthorized("Sign in first.")));
    };
    if !csrf_ok(headers, &session.csrf) {
        return Err(Box::new(unauthorized("Missing or wrong CSRF token.")));
    }
    if !settings.is_this_computer(peer.ip(), headers) {
        return Err(Box::new(json_error(
            StatusCode::FORBIDDEN,
            "Setup is done only on this computer.",
        )));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
pub(in crate::settings_server) struct Ask {
    #[serde(default)]
    fresh: Option<String>,
}

pub(in crate::settings_server) async fn get_wizard(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(ask): Query<Ask>,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    let can_act = settings.is_this_computer(peer.ip(), &headers);
    let force = ask.fresh.as_deref().is_some_and(|f| f == "1" || f == "true");
    let facts = settings.wizard_facts(can_act, force).await;
    let mut all = steps::steps(&facts);
    all[0].data["target"] = json!(settings.link_target());
    let complete = steps::complete(&all);
    settings.note_complete(complete);
    Json(json!({
        "complete": complete,
        "current": steps::current(&all),
        "canAct": can_act,
        "computer": facts.computer,
        "storageProblem": facts.storage_problem,
        "steps": all.iter().map(Step::json).collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(in crate::settings_server) struct TokenBody {
    #[serde(default)]
    token: String,
    #[serde(default)]
    kind: String,
}

/// Seals and writes one secret, then reads it back from disk: the value must open to the same fingerprint, or the save
/// did not take and the answer says so (the 2026-11-19 failure was a save nobody could see had not happened).
fn save_secret(settings: &SettingsServer, name: &str, value: &str) -> Result<(), Fail> {
    let Some(secrets) = settings.secrets.get() else {
        return Err(Box::new(internal_error("This agent can't save secrets from the page.")));
    };
    if let Some(why) = settings.storage_problem() {
        return Err(Box::new(internal_error(&why)));
    }
    let plan = secrets
        .plan(&one_secret(name, value))
        .map_err(|e| Box::new(bad_request(&e.0)))?;
    secrets
        .commit(plan)
        .map_err(|e| Box::new(internal_error(&format!("Couldn't save it: {e:#}"))))?;
    let read_back = secrets
        .states()
        .into_iter()
        .find(|s| s.name == name)
        .and_then(|s| s.fingerprint);
    if read_back.as_deref() != Some(&fingerprint(value)) {
        return Err(Box::new(internal_error(
            "It was written but could not be read back from the data folder: the key and the saved value disagree. Paste it again.",
        )));
    }
    settings.wizard.forget();
    Ok(())
}

pub(in crate::settings_server) async fn post_github(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = may_change(&settings, &peer, &headers) {
        return *r;
    }
    let Ok(TokenBody { token, .. }) = serde_json::from_slice(&body) else {
        return bad_request("No JSON body with a token.");
    };
    if let Err(r) = save_secret(&settings, "GH_TOKEN", token.trim()) {
        return *r;
    }
    answer(&settings, "github").await
}

pub(in crate::settings_server) async fn post_claude(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = may_change(&settings, &peer, &headers) {
        return *r;
    }
    let Ok(TokenBody { token, kind }) = serde_json::from_slice(&body) else {
        return bad_request("No JSON body with a token and its kind.");
    };
    let name = match kind.as_str() {
        "token" => CLAUDE_SECRETS[0],
        "key" => CLAUDE_SECRETS[1],
        _ => return bad_request("kind is token or key."),
    };
    if let Err(r) = save_secret(&settings, name, token.trim()) {
        return *r;
    }
    answer(&settings, "claude").await
}

/// What the page shows right after a save: the step, checked now.
async fn answer(settings: &SettingsServer, id: &str) -> Response {
    let facts = settings.wizard_facts(true, true).await;
    let all = steps::steps(&facts);
    let step = all.iter().find(|s| s.id == id).map_or(Value::Null, Step::json);
    Json(json!({ "saved": true, "step": step })).into_response()
}

pub(in crate::settings_server) async fn post_claude_test(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = may_change(&settings, &peer, &headers) {
        return *r;
    }
    match settings.probes().claude_test().await {
        Ok(said) => Json(json!({ "ok": true, "said": said })).into_response(),
        Err(why) => Json(json!({ "ok": false, "why": why })).into_response(),
    }
}

#[derive(Deserialize)]
pub(in crate::settings_server) struct CiBody {
    off: bool,
}

/// Turns the CI runner off (its slots to 0) or back on (the default), in its own settings file like the programs
/// panel's save does.
pub(in crate::settings_server) async fn post_ci(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(r) = may_change(&settings, &peer, &headers) {
        return *r;
    }
    let Ok(CiBody { off }) = serde_json::from_slice(&body) else {
        return bad_request("No JSON body with off true or false.");
    };
    let mut update = ProgramUpdate::default();
    update
        .settings
        .insert("slots".to_string(), if off { json!(0) } else { Value::Null });
    match settings.save_program("ci-runner", &update, true) {
        Ok(()) => Json(json!({ "saved": true, "off": off })).into_response(),
        Err(response) => *response,
    }
}
