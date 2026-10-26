//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it only
//! answers `GET /api/health` on 127.0.0.1.
mod lanes;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use anyhow::{Context, Result};
use retaskable_agent::{api, cloud, config, store, tags};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(filter()).init();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let dir = config::data_dir(env);
    let cfg = config::load(&dir)?;

    // cloud/04-authentication.md section 4.2 / tray/02 decision 7: `retaskable-agent link
    // [environment]` links this machine to that cloud (or the config/env default), separate from
    // the server's normal startup below.
    let mut argv = std::env::args().skip(1);
    if argv.next().as_deref() == Some("link") {
        let link_args: Vec<String> = argv.collect();
        return run_link(&cfg, &dir, env, &link_args).await;
    }

    // agent-settings.md section 3.2: each computer-only setting's environment variable, read
    // here so it's logged the same way whether it came from the file or the environment.
    let lane_count = config::lanes(&cfg, env);
    for project in &cfg.projects {
        // Trims each project's own state.json on startup (local-state-diet.md); not fatal, since
        // a broken file there shouldn't keep the health check from coming up.
        let project_dir = dir.join("projects").join(project.id_or_derived());
        if let Err(e) = store::Store::load(&project_dir) {
            tracing::warn!("loading {}: {e:#}", project_dir.join("state.json").display());
        }
        // Keeps this project's lane pool at lane_count worktrees (04-architecture.md section
        // 4.1); not fatal, since a project not yet pointed at a real repo and worktrees folder
        // shouldn't keep the health check from coming up either.
        if !project.repo_path.is_empty() && !project.worktrees_dir.is_empty() {
            let repo_path = Path::new(&project.repo_path);
            let worktrees_dir = Path::new(&project.worktrees_dir);
            match lanes::ensure(repo_path, worktrees_dir, lane_count) {
                Ok(r) if !r.created.is_empty() || !r.removed.is_empty() => {
                    tracing::info!(
                        "{}: lane pool at {} (created {:?}, removed {:?})",
                        project.id_or_derived(),
                        worktrees_dir.display(),
                        r.created,
                        r.removed
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    "{}: lane pool at {}: {e:#}",
                    project.id_or_derived(),
                    worktrees_dir.display()
                ),
            }
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
    let poll_interval_sec = config::poll_interval_sec(&cfg, env);
    let local_model = config::local_model(&cfg, env);
    tracing::info!(
        "Settings: {lane_count} lane(s), checking every {poll_interval_sec}s, local model {}",
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

/// Links this machine to a cloud and keeps the refresh token it gets (cloud/04-authentication.md
/// section 4.2): requests a device code, shows the person where to go, polls until it's
/// approved, then saves the refresh token under this cloud's URL.
async fn run_link(
    cfg: &config::ConfigFile,
    dir: &Path,
    env: impl Fn(&str) -> Option<String>,
    link_args: &[String],
) -> Result<()> {
    let cloud_url = cloud::link_url(link_args, cfg, &env).map_err(anyhow::Error::msg)?;
    let machine = cloud::link::machine_name(&env, hostname);
    let os = cloud::link::os_name();

    let client = cloud::link::Client::new(cloud_url.clone());
    let token = cloud::link::link(&client, &machine, os, VERSION, |code| {
        println!("Go to {} and enter {}", code.verification_uri, code.user_code);
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let keyring = cloud::credentials::OsKeyring;
    let store = cloud::credentials::CredentialStore::detect(&keyring, dir, cfg!(target_os = "linux"));
    store.set(&keyring, &cloud_url, &token.refresh_token)?;

    println!("Linked to {cloud_url} as agent {}", token.agent_id);
    Ok(())
}

/// This machine's name for the device-code request: `COMPUTERNAME` on Windows, else the
/// `hostname` command's output.
fn hostname() -> Option<String> {
    if cfg!(target_os = "windows") {
        return std::env::var("COMPUTERNAME").ok().filter(|v| !v.is_empty());
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
