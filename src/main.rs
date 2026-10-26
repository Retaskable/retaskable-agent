//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it
//! answers `GET /api/health` on 127.0.0.1 and keeps its outbound link to each cloud.
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use retaskable_agent::{api, config, connector, lanes, link, phone, setup_check, store, tags, telemetry};

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
    // `retaskable link [production|staging|local]` (cli.ts `link`): link this computer to a
    // cloud in the terminal, then exit. A running agent picks the token up within a minute.
    if std::env::args().nth(1).as_deref() == Some("link") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(link_command(&args, &cfg, &dir, env).await);
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
    // cloud.md section 5 / environments.md section 3.4: one outbound link to each cloud, every
    // project on it. Unlinked clouds wait quietly for `retaskable link`.
    let keychain = link::keychain(&dir, env);
    if let Some(note) = link::credential_file_note(&dir, env) {
        tracing::info!("{note}");
    }
    let view = Arc::new(connector::AgentView {
        machine: link::machine_name(env),
        tags: tag_list,
        projects: cfg.projects.iter().map(connector::ProjectView::from).collect(),
    });
    let http = reqwest::Client::new();
    let connectors: Vec<_> = link::cloud_urls(&cfg, env)
        .iter()
        .map(|url| {
            connector::Connector::start(
                url,
                keychain.clone(),
                http.clone(),
                view.clone(),
                connector::Timing::default(),
            )
        })
        .collect();
    // first-run-check.md section 3.1: printed once the port is up, same as `agent/src/index.ts`.
    // Fire-and-forget so a slow or missing `gh`/`claude` doesn't delay serving `/api/health`.
    tokio::spawn(async move {
        let checks = setup_check::run_setup_checks(env).await;
        println!("{}", setup_check::setup_check_report(&checks));
    });
    axum::serve(listener, api::router(VERSION))
        .with_graceful_shutdown(shutdown())
        .await?;
    for c in connectors {
        c.stop(Duration::from_secs(10)).await;
    }
    Ok(())
}

/// `retaskable link [production|staging|local]`: the device flow in the terminal. The exit code.
async fn link_command(
    args: &[String],
    cfg: &config::ConfigFile,
    dir: &Path,
    env: impl Fn(&str) -> Option<String> + Copy,
) -> i32 {
    let url = match args {
        [] => link::cloud_url(cfg, env).map_err(|e| format!("{e:#}")),
        [name] => link::environment_url(name).map(String::from).ok_or_else(String::new),
        _ => Err(String::new()),
    };
    let url = match url {
        Ok(url) => url,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            eprintln!("Usage: retaskable link [production|staging|local]");
            return 2;
        }
    };
    let keychain = link::keychain(dir, env);
    if matches!(keychain.get(&url), Ok(Some(_))) {
        println!("Already linked to {url}. Linking again replaces that link.");
    }
    println!("Linking to {url}…");
    let flow = link::Link {
        http: reqwest::Client::new(),
        store: keychain.as_ref(),
        machine: link::machine_info(env),
        target: link::CLOUD_TARGET,
        second: Duration::from_secs(1),
    };
    let shown = |code: &link::DeviceCode| {
        println!("Go to {} and enter {}", code.verification_uri, code.user_code);
        if !code.verification_uri_complete.is_empty() {
            println!("(or open {})", code.verification_uri_complete);
        }
        println!(
            "The code works for {} minutes. Waiting for approval…",
            code.expires_in.div_ceil(60)
        );
    };
    match flow.run(&url, shown).await {
        Ok(id) => {
            println!("Linked to {url} as agent {id}. The refresh token is in this machine's credential store.");
            0
        }
        Err(e) => {
            eprintln!("{e:#}");
            1
        }
    }
}

fn filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_env("RETASKABLE_LOG").unwrap_or_else(|_| "info".into())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Stopping");
}
