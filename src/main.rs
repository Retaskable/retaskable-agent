//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it
//! answers `GET /api/health` on 127.0.0.1 and keeps its outbound link to each cloud.
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use retaskable_agent::settings_server::SettingsServer;
use retaskable_agent::{
    api, backlog, config, connector, engines, lanes, link, local_model, network, phone, release, setup_check, store,
    tags, telemetry,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    // installers/03-the-bundle-agent-bundle.md section 3.7: a release build's own `build.json`,
    // beside the executable, read before anything else so a bad one (section 3.8) stops it here
    // rather than it guessing at its own identity. A checkout has none, and runs as always.
    let exe_path = std::env::current_exe().ok();
    let build = match exe_path.as_deref().map(release::load_build) {
        Some(Err(problem)) => {
            eprintln!("Retaskable couldn't start: {problem}.");
            std::process::exit(1);
        }
        Some(Ok(build)) => build,
        None => None,
    };
    let version = build
        .as_ref()
        .map(|b| b.version.clone())
        .unwrap_or_else(|| VERSION.to_string());
    // `retaskable --version` (release.ts): a release build's own version, else this crate's.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{version}");
        return Ok(());
    }
    // `retaskable check` (first-run-check.md section 3.1, `npm run check`): print the setup
    // checks and exit without starting the server.
    if std::env::args().nth(1).as_deref() == Some("check") {
        let checks = setup_check::run_setup_checks(env).await;
        println!("{}", setup_check::setup_check_report(&checks));
        std::process::exit(if setup_check::setup_checks_pass(&checks) { 0 } else { 1 });
    }
    tracing_subscriber::fmt().with_env_filter(filter()).init();
    // installers/03-the-bundle-agent-bundle.md section 3.3: a release build keeps its files in
    // the OS's own place, not the `.retaskable` a checkout uses, unless `RETASKABLE_DATA_DIR`
    // says otherwise.
    let dir = match &build {
        Some(b) if env("RETASKABLE_DATA_DIR").is_none() => release::data_dir(b, env),
        _ => config::data_dir(env),
    };
    let cfg = config::load(&dir)?;
    // `retaskable link [production|staging|local]` (cli.ts `link`): link this computer to a
    // cloud in the terminal, then exit. A running agent picks the token up within a minute.
    if std::env::args().nth(1).as_deref() == Some("link") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(link_command(&args, &cfg, &dir, env, &version).await);
    }
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
    tracing::info!("Retaskable agent {version} on http://{addr}");
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
        version: version.clone(),
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
    // installers/04-every-agent-on-the-shared-backlog-shared.md: once this computer already holds a
    // retaskable.com link, the shared backlog's device flow starts by itself too, opening its own
    // `/link` page. Fire-and-forget so a slow or unreachable store doesn't delay serving `/api/health`;
    // it isn't tried again this run (a failed or expired code waits for the next start).
    if link::cloud_urls(&cfg, env)
        .iter()
        .any(|url| matches!(keychain.get(url), Ok(Some(_))))
    {
        tokio::spawn(auto_link_backlog(keychain.clone(), http.clone(), env, version.clone()));
    }
    // first-run-check.md section 3.1: printed once the port is up, same as `agent/src/index.ts`.
    // Fire-and-forget so a slow or missing `gh`/`claude` doesn't delay serving `/api/health`.
    tokio::spawn(async move {
        let checks = setup_check::run_setup_checks(env).await;
        println!("{}", setup_check::setup_check_report(&checks));
    });
    let settings = Arc::new(SettingsServer::new(
        dir.clone(),
        port,
        network_access.enabled,
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
        api::router(version.clone(), settings).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    for c in connectors {
        c.stop(Duration::from_secs(10)).await;
    }
    Ok(())
}

/// `retaskable link [production|staging|local]`, or `retaskable link backlog`
/// (installers/04-every-agent-on-the-shared-backlog-shared.md, `npm run link -- backlog`): the device
/// flow in the terminal. The exit code.
async fn link_command(
    args: &[String],
    cfg: &config::ConfigFile,
    dir: &Path,
    env: impl Fn(&str) -> Option<String> + Copy,
    version: &str,
) -> i32 {
    if args == [String::from("backlog")] {
        let keychain = link::keychain(dir, env);
        let url = backlog::url(env);
        return match run_link_in_terminal(&keychain, &url, link::BACKLOG_TARGET, env, version).await {
            Ok(id) => {
                println!(
                    "Linked to the shared backlog as agent {id}. The refresh token is in this machine's credential store."
                );
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
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
            eprintln!("Usage: retaskable link [production|staging|local] | backlog");
            return 2;
        }
    };
    let keychain = link::keychain(dir, env);
    match run_link_in_terminal(&keychain, &url, link::CLOUD_TARGET, env, version).await {
        Ok(id) => {
            println!("Linked to {url} as agent {id}. The refresh token is in this machine's credential store.");
            // installers/04-every-agent-on-the-shared-backlog-shared.md: the shared backlog's own
            // approval comes right after this one, opened by the agent itself.
            auto_link_backlog(keychain.clone(), reqwest::Client::new(), env, version.to_string()).await;
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Runs one device-flow link against `target` at `url` in the terminal, storing the refresh token.
async fn run_link_in_terminal(
    keychain: &Arc<dyn link::TokenStore>,
    url: &str,
    target: link::LinkTarget,
    env: impl Fn(&str) -> Option<String> + Copy,
    version: &str,
) -> Result<u64, String> {
    if matches!(keychain.get(url), Ok(Some(_))) {
        println!("Already linked to {url}. Linking again replaces that link.");
    }
    println!("Linking to {url}…");
    let flow = link::Link {
        http: reqwest::Client::new(),
        store: keychain.as_ref(),
        machine: link::machine_info(env, version),
        target,
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
    flow.run(url, shown).await.map_err(|e| format!("{e:#}"))
}

/// installers/04-every-agent-on-the-shared-backlog-shared.md: starts the shared backlog's device flow by
/// itself, printing and opening its `/link` page the moment a code is ready. Logs the outcome instead of
/// failing the caller — a failed or expired code waits for the next start, or `retaskable link backlog`.
async fn auto_link_backlog(
    keychain: Arc<dyn link::TokenStore>,
    http: reqwest::Client,
    env: impl Fn(&str) -> Option<String> + Copy,
    version: String,
) {
    let shown = |code: &link::DeviceCode| {
        println!("Approve the shared backlog: {}", code.verification_uri_complete);
        if let Err(e) = link::open_url(&code.verification_uri_complete) {
            tracing::warn!("couldn't open the shared backlog's link page: {e:#}");
        }
    };
    match backlog::auto_link(keychain, http, env, &version, shown).await {
        Ok(Some(id)) => tracing::info!("Linked to the shared backlog as agent {id}."),
        Ok(None) => {}
        Err(e) => tracing::warn!("Couldn't link to the shared backlog: {e:#}"),
    }
}

fn filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_env("RETASKABLE_LOG").unwrap_or_else(|_| "info".into())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Stopping");
}
