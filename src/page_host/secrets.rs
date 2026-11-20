//! The programs' secrets as the page shows them (agent-settings.md section 3.3), and who may save one. Nothing here
//! ever returns or logs a value.
use std::net::IpAddr;

use axum::http::{HeaderMap, header};
use serde_json::{Value, json};

use super::PageHost;
use super::auth::split_host_port;
use crate::secrets::Secrets;

/// Each secret's state as the page shows it: name, label, use, source and fingerprint; never the value.
pub(crate) fn secret_rows(secrets: &Secrets) -> Vec<Value> {
    secrets
        .states()
        .into_iter()
        .map(|s| {
            let mut row = json!({
                "name": s.name,
                // A variable a person added has no label or use; the page shows its name.
                "custom": s.def.is_none(),
                "label": s.def.map(|d| d.label),
                "use": s.def.map(|d| d.usage),
                "source": s.source.as_str(),
                // What deleting the saved one goes back to: the environment's value, or nothing.
                "envSet": s.env_set,
                // The CI runner's token is the code agent's: it has none of its own.
                "inherited": s.inherited,
            });
            if let Some(print) = s.fingerprint {
                row["fingerprint"] = json!(print);
            }
            row
        })
        .collect()
}

impl PageHost {
    /// Section 4.3: a request from a loopback address or the trusted Docker gateway (`docker_trust.rs`), or, with `RETASKABLE_SETTINGS_PUBLISHED_LOCAL=1` (a port Docker
    /// publishes on the host's 127.0.0.1 only, so every request reaches the agent from Docker's gateway), one whose
    /// Host is `127.0.0.1`, `localhost` or `[::1]`. A token saved from any other device would cross the network in
    /// plain HTTP.
    pub(crate) fn is_this_computer(&self, peer: IpAddr, headers: &HeaderMap) -> bool {
        if self.is_local_peer(peer) {
            return true;
        }
        if self
            .env
            .get("RETASKABLE_SETTINGS_PUBLISHED_LOCAL")
            .map(|v| v.trim() == "1")
            != Some(true)
        {
            return false;
        }
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let (name, _) = split_host_port(host);
        matches!(
            name.to_ascii_lowercase().as_str(),
            "127.0.0.1" | "localhost" | "[::1]" | "::1"
        )
    }
}
