//! The settings page's secrets (agent-settings.md section 3.3): their states in `GET /api/settings`, and `secrets` in
//! `POST /api/settings`, accepted only from this computer. Nothing here ever returns or logs a value.
use std::net::IpAddr;
use std::sync::Arc;

use axum::http::HeaderMap;
use axum::response::Response;
use serde_json::{Map, Value, json};

use super::SettingsServer;
use crate::page_host::{bad_request, json_error, secret_rows};
use crate::secrets::{Plan, Secrets};

use axum::http::StatusCode;

impl SettingsServer {
    /// Gives the page the running agent's secrets; with none, `GET /api/settings` has no `secrets` and a save of any is refused.
    pub fn set_secrets(&self, secrets: Arc<Secrets>) {
        let _ = self.secrets.set(secrets);
    }

    /// Adds each secret's state (name, label, use, source and fingerprint; never the value), whether this request may
    /// save them, and why a save can't work here when it can't.
    pub(super) fn add_secrets(&self, view: &mut Value, peer: IpAddr, headers: &HeaderMap) {
        let Some(secrets) = self.secrets.get() else {
            return;
        };
        view["secrets"] = Value::Array(secret_rows(secrets));
        view["canSaveSecrets"] = json!(self.is_this_computer(peer, headers));
        view["secretsNote"] = json!(secrets.save_problem());
    }

    /// Checks the body's `secrets` (None when it has none) and seals it, writing nothing, so the rest of the body can be
    /// checked before any of it is saved.
    pub(super) fn plan_secrets(
        &self,
        body: Option<&Value>,
        peer: IpAddr,
        headers: &HeaderMap,
    ) -> Result<Option<Plan>, Box<Response>> {
        let Some(body) = body else {
            return Ok(None);
        };
        let secrets = self.secrets.get().filter(|_| self.is_this_computer(peer, headers));
        let Some(secrets) = secrets else {
            return Err(Box::new(json_error(
                StatusCode::FORBIDDEN,
                "Secrets can be saved only on this computer.",
            )));
        };
        let Some(changes) = body.as_object() else {
            return Err(Box::new(bad_request("secrets must be an object.")));
        };
        secrets
            .plan(changes as &Map<String, Value>)
            .map(Some)
            .map_err(|e| Box::new(bad_request(&e.0)))
    }

    /// Writes what `plan_secrets` checked.
    pub(super) fn commit_secrets(&self, plan: Option<Plan>) -> Result<(), Box<Response>> {
        match (plan, self.secrets.get()) {
            (Some(plan), Some(secrets)) => secrets
                .commit(plan)
                .map_err(|e| Box::new(super::internal_error(&format!("{e:#}")))),
            _ => Ok(()),
        }
    }
}
