//! The port guard: `claim` mode must not claim while the TypeScript agent runs on this computer, since
//! both would claim from the same backlog and take the same checkouts. The TS agent has no lock file; the
//! one trace it leaves is its port, which answers `GET /api/health` (`createApi` in `agent/src/api.ts`).
//! Its answer, `{"ok":true,"version":..}`, is the same body this agent gives, so it can't be told apart
//! by content: an answer on any port but our own counts as another agent. Our own port is skipped by
//! number, since our own server answers there.
use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::Value;

/// How long one probe may take. A loopback listener that is up answers at once.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Which ports another agent could be on, and our own.
#[derive(Debug, Clone)]
pub struct PortGuard {
    candidates: BTreeSet<u16>,
    client: reqwest::Client,
}

impl PortGuard {
    /// `ports` are where the TS agent may listen: its default for this build, the shared default, and the
    /// port the config file names (the TS agent reads the same `port` setting and `RETASKABLE_PORT`, so a
    /// differing environment is the one case this cannot see). `own_port` is the one our server is bound to.
    pub fn new(own_port: u16, ports: impl IntoIterator<Item = u16>) -> Self {
        Self {
            candidates: ports.into_iter().filter(|&p| p != own_port).collect(),
            client: reqwest::Client::builder()
                .timeout(PROBE_TIMEOUT)
                .build()
                .unwrap_or_default(),
        }
    }

    /// The port another agent answers `GET /api/health` on, if any. Only a 200 whose body says `ok` counts;
    /// a closed port, a timeout or any other answer means nothing is there.
    pub async fn other_agent(&self) -> Option<u16> {
        for &port in &self.candidates {
            let url = format!("http://127.0.0.1:{port}/api/health");
            let Ok(res) = self.client.get(&url).send().await else {
                continue;
            };
            if !res.status().is_success() {
                continue;
            }
            if let Ok(body) = res.json::<Value>().await
                && body["ok"] == Value::Bool(true)
            {
                return Some(port);
            }
        }
        None
    }

    /// Why claiming is refused right now, or `None` when it may go ahead. Cheap enough to ask before every claim.
    pub async fn refusal(&self) -> Option<String> {
        let port = self.other_agent().await?;
        Some(format!(
            "another Retaskable agent answers on port {port} (the TypeScript agent?), so this one will not claim work"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::get};
    use serde_json::json;

    /// A listener standing in for the TS agent's port.
    async fn listener_answering(health: Router) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, health).await.unwrap() });
        port
    }

    fn ts_agent() -> Router {
        Router::new().route(
            "/api/health",
            get(|| async { Json(json!({ "ok": true, "version": "1.2.3" })) }),
        )
    }

    /// A port nothing listens on.
    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn an_agent_answering_health_on_another_port_blocks_claiming() {
        let ts = listener_answering(ts_agent()).await;
        let guard = PortGuard::new(closed_port(), [ts]);
        assert_eq!(guard.other_agent().await, Some(ts));
        assert!(guard.refusal().await.unwrap().contains(&ts.to_string()));
    }

    #[tokio::test]
    async fn nothing_listening_lets_claiming_go_ahead() {
        let guard = PortGuard::new(closed_port(), [closed_port(), closed_port()]);
        assert_eq!(guard.refusal().await, None);
    }

    #[tokio::test]
    async fn our_own_port_is_never_another_agent() {
        let own = listener_answering(ts_agent()).await;
        let guard = PortGuard::new(own, [own]);
        assert_eq!(guard.other_agent().await, None);
    }

    #[tokio::test]
    async fn something_else_on_the_port_is_not_an_agent() {
        let not_found = listener_answering(Router::new()).await;
        let not_ok = listener_answering(Router::new().route(
            "/api/health",
            get(|| async { (StatusCode::OK, Json(json!({ "ok": false }))) }),
        ))
        .await;
        let not_json = listener_answering(Router::new().route("/api/health", get(|| async { "hello" }))).await;
        let guard = PortGuard::new(closed_port(), [not_found, not_ok, not_json]);
        assert_eq!(guard.other_agent().await, None);
    }
}
