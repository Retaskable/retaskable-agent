//! The Retaskable agent, in Rust: a port of Retaskable.com's `agent/` (TypeScript), following
//! docs/specs/agent.md there. So far it only answers `GET /api/health` on 127.0.0.1.
mod api;
mod config;
mod store;
mod tags;

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};

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
    // agent-settings.md section 3 / computer-tags.md section 3.1: tags from the machine, with
    // machine.json's overrides and RETASKABLE_TAGS over them. Missing is fine; broken is fatal,
    // the same as config.json.
    let machine = store::MachineStore::load(&dir)?;
    let found = tags::found_tags(&tags::machine_facts(env));
    let overrides = machine.tags();
    let tag_list = tags::effective_tags(&found, overrides.as_ref(), env);
    tracing::info!(
        "Tags: {}",
        if tag_list.is_empty() {
            "none".to_string()
        } else {
            tag_list.join(", ")
        }
    );
    // agent-settings.md section 3.2: each computer-only setting's environment variable, read
    // here so it's logged the same way whether it came from the file or the environment.
    let lanes = config::lanes(&cfg, env);
    let poll_interval_sec = config::poll_interval_sec(&cfg, env);
    let local_model = config::local_model(&cfg, env);
    tracing::info!(
        "Settings: {lanes} lane(s), checking every {poll_interval_sec}s, local model {}",
        if local_model.enabled {
            format!("{} at {}", local_model.model, local_model.base_url)
        } else {
            "off".to_string()
        }
    );
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
