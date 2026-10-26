//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it only
//! answers `GET /api/health` on 127.0.0.1.
mod lanes;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use anyhow::{Context, Result};
use retaskable_agent::{api, config, setup_check, store, tags, telemetry};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    // `retaskable check` (first-run-check.md section 3.1, `npm run check`): print the setup
    // checks and exit without starting the server.
    if std::env::args().nth(1).as_deref() == Some("check") {
        let checks = setup_check::run_setup_checks(env).await;
        println!("{}", setup_check::setup_check_report(&checks));
        std::process::exit(if setup_check::setup_checks_pass(&checks) { 0 } else { 1 });
    }
    tracing_subscriber::fmt().with_env_filter(filter()).init();
    let dir = config::data_dir(env);
    let cfg = config::load(&dir)?;
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
        // Opens this project's event log (agent-telemetry.md section 3.1), now that it's
        // configured with a real id: prunes files past KEEP_DAYS and reads the last day back.
        // Nothing calls `EventLog::add` yet — that needs the orchestrator loop, sessions and the
        // backlog client wired in first, none of which this agent runs yet.
        telemetry::EventLog::new().open(&project_dir.join("events"));
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
    // agent-settings.md section 3.1: which program runs each kind of work, and Cursor's model
    // per tier. Not used to run a session yet (engines.md's own port), so logged for now the
    // same way the settings above are before anything reads them.
    let engine_choice = config::engine_choice(&cfg, env);
    let cursor_models = config::cursor_models(&cfg, env);
    tracing::info!(
        "Engines: default {}, lanes [{}], Cursor models top {}, implementer {}, scout {}",
        engine_choice.default.unwrap_or(config::Engine::Claude),
        engine_choice
            .lanes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        cursor_models.top.as_deref().unwrap_or("auto"),
        cursor_models.implementer.as_deref().unwrap_or("auto"),
        cursor_models.scout.as_deref().unwrap_or("auto"),
    );
    // Only this computer: the agent never listens on a public port (agent.md section 8).
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config::port(&cfg, env)));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    tracing::info!("Retaskable agent {VERSION} on http://{addr}");
    // first-run-check.md section 3.1: printed once the port is up, same as `agent/src/index.ts`.
    // Fire-and-forget so a slow or missing `gh`/`claude` doesn't delay serving `/api/health`.
    tokio::spawn(async move {
        let checks = setup_check::run_setup_checks(env).await;
        println!("{}", setup_check::setup_check_report(&checks));
    });
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
