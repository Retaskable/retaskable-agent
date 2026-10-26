//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it only
//! answers `GET /api/health` on 127.0.0.1.
use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use retaskable_agent::{api, config, store};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(filter()).init();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let dir = config::data_dir(env);
    let cfg = config::load(&dir)?;
    // Trims each project's own state.json on startup (local-state-diet.md); not fatal, since
    // a broken file there shouldn't keep the health check from coming up.
    for project in &cfg.projects {
        let project_dir = dir.join("projects").join(project.id_or_derived());
        if let Err(e) = store::Store::load(&project_dir) {
            tracing::warn!("loading {}: {e:#}", project_dir.join("state.json").display());
        }
    }
    // Only this computer: the agent never listens on a public port (agent.md section 8).
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config::port(&cfg, env)));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    tracing::info!("Retaskable agent {VERSION} on http://{addr}");
    axum::serve(listener, api::router(VERSION))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

fn filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_env("RETASKABLE_LOG").unwrap_or_else(|_| "info".into())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Stopping");
}
