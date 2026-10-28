//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it only
//! answers `GET /api/health` on 127.0.0.1.
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use retaskable_agent::settings_server::SettingsServer;
use retaskable_agent::{
    api, config, engines, lanes, local_model, network, phone, setup_check, store, tags, telemetry,
};

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
    // agent-settings.md section 3.1: which program runs each kind of work (engines.md section
    // 3.1), looked up per project below since a project's older `engines.default`/`roles` still
    // counts until this computer saves its own choice (`legacy_choice`, `engines::engine_for`).
    let engine_choice = config::engine_choice(&cfg, env);
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
        // play-on-phone.md section 3: while this project has a `phone` section, watch for a
        // plugged-in, authorised Android phone. Only adb is looked up at start; re-finding it
        // when Setup is saved needs the settings server, not yet ported.
        if project.phone.is_some() {
            match phone::find_adb_on_path(env) {
                Some(adb) => {
                    tracing::info!(
                        "{}: watching for an Android phone with {}",
                        project.id_or_derived(),
                        adb.display()
                    );
                    tokio::spawn(phone::watch(adb));
                }
                None => tracing::warn!("{}: {}", project.id_or_derived(), phone::NO_ADB),
            }
        }
        // Which program runs each kind of work in this project, once this computer's own
        // `engineChoice` (or, until it has one, this project's older `engines.default`/`roles`)
        // is resolved per role group. Not used to run a session yet: that needs the orchestrator
        // loop, still a later item.
        let legacy = project.engines.as_ref();
        tracing::info!(
            "{}: engines planner {}, implementer {}, review {}, drafter {}, chat {}, writer {}",
            project.id_or_derived(),
            engines::engine_for(&engine_choice, legacy, engines::Role::Planner, None),
            engines::engine_for(&engine_choice, legacy, engines::Role::Implementer, None),
            engines::engine_for(&engine_choice, legacy, engines::Role::Scout, None),
            engines::engine_for(&engine_choice, legacy, engines::Role::Drafter, None),
            engines::engine_for(&engine_choice, legacy, engines::Role::Assistant, None),
            engines::engine_for(&engine_choice, legacy, engines::Role::Writer, None),
        );
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
    // 07-orchestrator-loop.md section 7.2: while local is on, watch whether it's up, once per
    // computer (not per project) — a local session running against it is a later slice, since no
    // orchestrator exists yet to start one.
    if local_model.enabled {
        tracing::info!("Watching the local model at {}", local_model.base_url);
        tokio::spawn(local_model::watch(local_model.base_url.clone()));
    }
    // agent-settings.md section 3.1: the computer-wide shape of `engineChoice` (resolved per
    // project and role group above), and Cursor's model per tier. Cursor's models aren't read by
    // anything yet (engines.md section 3.4, a later slice), so still only logged.
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
    // agent-settings.md section 4.4: off by default, 127.0.0.1 only; on with a password, every
    // address instead, so another computer on the local network can reach the settings page.
    let network_access = network::network_access(machine.network().as_ref(), env);
    let host = if network_access.enabled {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    // Only this computer: the agent never listens on a public port (agent.md section 8).
    let port = config::port(&cfg, env);
    let addr = SocketAddr::from((host, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    if network_access.enabled {
        tracing::info!("The settings page is open on the network, at http://{addr}");
    }
    tracing::info!("Retaskable agent {VERSION} on http://{addr}");
    // first-run-check.md section 3.1: printed once the port is up, same as `agent/src/index.ts`.
    // Fire-and-forget so a slow or missing `gh`/`claude` doesn't delay serving `/api/health`.
    tokio::spawn(async move {
        let checks = setup_check::run_setup_checks(env).await;
        println!("{}", setup_check::setup_check_report(&checks));
    });
    let settings = Arc::new(SettingsServer::new(
        dir.clone(),
        port,
        env,
        || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        // Restarting answers the request first: the process exit happens on another thread, a
        // moment later (agent/08-http-api.md: a restart finishes a merge first; this agent has
        // none in flight on its own HTTP thread, but the response still deserves to flush).
        |code: i32| {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                std::process::exit(code);
            });
        },
    ));
    axum::serve(
        listener,
        api::router(VERSION, settings).into_make_service_with_connect_info::<SocketAddr>(),
    )
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
