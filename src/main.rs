//! The Retaskable agent's binary: wires `retaskable_agent`'s pieces together. So far it
//! answers `GET /api/health` on 127.0.0.1 and keeps its outbound link to each cloud.
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use retaskable_agent::env::Env;
use retaskable_agent::settings_server::{SettingsServer, StatusSource};
use retaskable_agent::{
    api, backlog, config, connector, lane_pool, link, local_model, network, orchestrator, release, runtime,
    setup_check, store, tags, telemetry, view,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    let env = Env::from_process();
    let env = &env;
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
    // installers/03-the-bundle-agent-bundle.md section 3.3: a release build keeps its files in
    // the OS's own place, not the `.retaskable` a checkout uses, unless `RETASKABLE_DATA_DIR`
    // says otherwise.
    let dir = match &build {
        Some(b) if env.get("RETASKABLE_DATA_DIR").is_none() => release::data_dir(b, env),
        _ => config::data_dir(env),
    };
    // agent-telemetry.md section 5: the three commands for reading the event log and the session
    // transcripts back. Each reads straight from the data dir (a release build's own,
    // section 3.3 of the bundle spec) and exits without starting the server.
    if std::env::args().nth(1).as_deref() == Some("why") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        let Some(item) = telemetry::find_item(&args) else {
            eprintln!("Usage: retaskable why -- <item id> [--since <ISO time | 90m | 6h | 2d>] [--project <id>]");
            std::process::exit(2);
        };
        let since = match telemetry::flag(&args, "since") {
            Some(text) => match telemetry::parse_since(&text, telemetry::now_ms()) {
                Some(ms) => ms,
                None => {
                    eprintln!("--since {text}: give an ISO time or a span such as 90m, 6h or 2d.");
                    std::process::exit(2);
                }
            },
            None => 0,
        };
        let events = telemetry::read_all(&dir, since, i64::MAX, telemetry::flag(&args, "project").as_deref());
        println!("{}", telemetry::why_text(&events, &item));
        std::process::exit(0);
    }
    if std::env::args().nth(1).as_deref() == Some("agent-report") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        let hours = match telemetry::flag(&args, "hours") {
            Some(text) => text.parse::<f64>().ok(),
            None => Some(24.0),
        };
        let Some(hours) = hours.filter(|h| *h > 0.0) else {
            eprintln!("Usage: retaskable agent-report -- [--hours 24] [--project <id>]");
            std::process::exit(2);
        };
        let to = telemetry::now_ms();
        let from = to - (hours * 3_600_000.0) as i64;
        let events = telemetry::read_all(&dir, from, to, telemetry::flag(&args, "project").as_deref());
        println!("{}", telemetry::report_text(&events, from, to));
        std::process::exit(0);
    }
    if std::env::args().nth(1).as_deref() == Some("start-report") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        let sessions = match telemetry::flag(&args, "sessions") {
            Some(text) => text.parse::<f64>().ok(),
            None => Some(20.0),
        };
        let Some(sessions) = sessions.filter(|s| *s > 0.0) else {
            eprintln!("Usage: retaskable start-report -- [--sessions 20] [--project <id>]");
            std::process::exit(2);
        };
        let stats = telemetry::read_start_stats(&dir, sessions as usize, telemetry::flag(&args, "project").as_deref());
        println!("{}", telemetry::start_stats_text(&stats));
        std::process::exit(0);
    }
    // installers/03-the-bundle-agent-bundle.md section 3.7: a release build has no terminal, so
    // `console.log`/`info`/`debug`/`warn`/`error` go to `retaskable.log` in its log folder
    // instead, rotated at 10 MB with 3 old ones kept; a checkout logs to its terminal as before.
    // The first line names the build, and an uncaught panic is logged with its stack.
    match &build {
        Some(b) => {
            let logs_dir = release::logs_dir(b, env);
            match release::RotatingLog::open(&logs_dir) {
                Ok(log) => tracing_subscriber::fmt()
                    .with_env_filter(filter())
                    .with_ansi(false)
                    .with_writer(log)
                    .init(),
                Err(e) => {
                    tracing_subscriber::fmt().with_env_filter(filter()).init();
                    tracing::warn!("couldn't open {}: {e}", logs_dir.join("retaskable.log").display());
                }
            }
            tracing::info!("{}", release::build_line(b));
            std::panic::set_hook(Box::new(|info| {
                tracing::error!(
                    "Retaskable crashed: {info}\n{}",
                    std::backtrace::Backtrace::force_capture()
                );
            }));
        }
        None => tracing_subscriber::fmt().with_env_filter(filter()).init(),
    }
    let cfg = config::load(&dir)?;
    // `retaskable link [production|staging|local]` (cli.ts `link`): link this computer to a
    // cloud in the terminal, then exit. A running agent picks the token up within a minute.
    if std::env::args().nth(1).as_deref() == Some("link") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(link_command(&args, &cfg, &dir, env, &version, build.as_ref()).await);
    }
    // agent-settings.md section 3.2: each computer-only setting's environment variable, read
    // here so it's logged the same way whether it came from the file or the environment.
    let lane_count = config::lanes(&cfg, env);
    // agent-settings.md section 3.1: which program runs each kind of work (engines.md section
    // 3.1), looked up per project below since a project's older `engines.default`/`roles` still
    // counts until this computer saves its own choice (`legacy_choice`, `engines::engine_for`).
    let engine_choice = config::engine_choice(&cfg, env);
    for project in &cfg.projects {
        runtime::start_project(&dir, lane_count, &engine_choice, project, env);
    }
    // agent-settings.md section 3 / computer-tags.md section 3.1: tags from the machine, with
    // machine.json's overrides and RETASKABLE_TAGS over them. Missing is fine; broken is fatal,
    // the same as config.json.
    let machine = store::MachineStore::load(&dir)?;
    let tag_list = log_runtime_settings(&cfg, &machine, &engine_choice, lane_count, env);
    // agent-settings.md section 4.4: off by default, 127.0.0.1 only; on with a password, every
    // address instead, so another computer on the local network can reach the settings page.
    let network_access = network::network_access(machine.network().as_ref(), env);
    let host = if network_access.enabled {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    // Only this computer: the agent never listens on a public port (agent.md section 8).
    // installers/03-the-bundle-agent-bundle.md section 3.13: a Staging release build's own
    // default differs from Production's and a checkout's.
    let default_port = release::default_port(build.as_ref());
    let port = config::port(&cfg, env, default_port);
    let addr = SocketAddr::from((host, port));
    let listener = bind_with_retry(addr, BIND_RETRY_INTERVAL, BIND_RETRY_TIMEOUT)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    if network_access.enabled {
        tracing::info!("The settings page is open on the network, at http://{addr}");
    }
    tracing::info!("Retaskable agent {version} on http://{addr}");
    serve(
        ServeArgs {
            dir: &dir,
            cfg: &cfg,
            version: &version,
            build: build.as_ref(),
            exe_path: exe_path.as_deref(),
            tag_list,
            network_enabled: network_access.enabled,
            port,
            default_port,
            listener,
        },
        env,
    )
    .await
}

/// How often [`bind_with_retry`] tries again, and how long it keeps trying before it fails.
const BIND_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const BIND_RETRY_TIMEOUT: Duration = Duration::from_secs(15);

/// `TcpListener::bind`, retrying while it fails with `AddrInUse`: installers/03-the-bundle-agent-
/// bundle.md section 3.8 says a relaunched copy "waits for the port as after any restart", since
/// the copy it replaces starts it before giving up its own listening socket (the macOS/Linux
/// installers' restart and this binary's own relaunch fallback both spawn the next copy first,
/// then exit). Any other error fails at once, as before.
async fn bind_with_retry(
    addr: SocketAddr,
    retry_every: Duration,
    give_up_after: Duration,
) -> std::io::Result<tokio::net::TcpListener> {
    let started = std::time::Instant::now();
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && started.elapsed() < give_up_after => {
                tokio::time::sleep(retry_every).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// What [`serve`] needs to run the server, bundled so the function stays under clippy's
/// argument-count limit.
struct ServeArgs<'a> {
    dir: &'a Path,
    cfg: &'a config::ConfigFile,
    version: &'a str,
    build: Option<&'a release::Build>,
    exe_path: Option<&'a Path>,
    tag_list: Vec<String>,
    network_enabled: bool,
    port: u16,
    default_port: u16,
    listener: tokio::net::TcpListener,
}

/// Runs each cloud's outbound connector, the shared backlog's auto-link, the first-run check,
/// and the settings/health HTTP server, until the server shuts down.
async fn serve(args: ServeArgs<'_>, env: &Env) -> Result<()> {
    let ServeArgs {
        dir,
        cfg,
        version,
        build,
        exe_path,
        tag_list,
        network_enabled,
        port,
        default_port,
        listener,
    } = args;
    // Owned, not borrowed: the restart fallback below needs it in a thread that outlives `args`.
    let exe_path = exe_path.map(Path::to_path_buf);
    let is_release = build.is_some();
    // cloud.md section 5 / environments.md section 3.4: one outbound link to each cloud, every
    // project on it. Unlinked clouds wait quietly for `retaskable link`.
    let keychain = link::keychain(dir, env);
    if let Some(note) = link::credential_file_note(dir, env) {
        tracing::info!("{note}");
    }
    let lane_count = config::lanes(cfg, env);
    let view = Arc::new(agent_view(cfg, tag_list.clone(), version, env));
    runtime::spawn_lane_size_pollers(&cfg.projects, &view.lane_sizes, lane_count);
    let http = reqwest::Client::new();
    let cloud_urls = link::cloud_urls(cfg, env, build);
    let updater_slot = runtime::UpdaterSlot::default();
    let exit = runtime::restart_handler(is_release, exe_path.clone(), updater_slot.clone());
    let connectors = link_clouds(&cloud_urls, &keychain, &http, &view, &exit, dir, version, env);
    let latest_checks = runtime::spawn_setup_checks(is_release, env.clone());
    // The orchestrator's safety switch: off unless RETASKABLE_ORCHESTRATOR says dry (or claim, which
    // isn't built and runs dry). Read and logged once, here.
    // Where the TS agent may be listening: its default for this build, the shared default and the
    // configured port. `claim` runs dry instead while one of them answers (port guard, guard.rs).
    let guard = orchestrator::guard::PortGuard::new(
        port,
        [
            default_port,
            config::DEFAULT_PORT,
            cfg.machine.port.unwrap_or(default_port),
        ],
    );
    let mode = orchestrator::driver::OrchestratorMode::read_and_log(env);
    let mode = orchestrator::driver::effective_mode(mode, &guard).await;
    let live = orchestrator::live::LiveHandle::new(mode);
    orchestrator::driver::spawn_all(orchestrator::driver::SpawnArgs {
        dir,
        projects: &cfg.projects,
        lane_count,
        concurrency: cfg.machine.concurrency,
        poll_every: std::time::Duration::from_secs(config::poll_interval_sec(cfg, env).into()),
        backlog_url: &backlog::url(env),
        keychain: &keychain,
        paused: &view.paused,
        live: &live,
        guard: (mode == orchestrator::driver::OrchestratorMode::Claim).then_some(&guard),
        machine: link::machine_name(env),
        // `claim_next` is told the host of the retaskable.com this computer links to (`storeAgent` in the TS agent).
        environment: link::cloud_url(cfg, env, build)
            .ok()
            .and_then(|u| reqwest::Url::parse(&u).ok())
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default(),
        tags: tag_list,
        env: env.clone(),
        engine_choice: config::engine_choice(cfg, env),
        starter: (mode == orchestrator::driver::OrchestratorMode::Claim).then(|| {
            orchestrator::session::ClaudeStarter::new(
                dir.to_path_buf(),
                cfg.projects.clone(),
                config::engine_choice(cfg, env),
                env.clone(),
                cfg.machine.max_turns,
                cfg.machine.models,
            ) as Arc<dyn orchestrator::driver::SessionStarter>
        }),
    });
    if let Some(b) = build {
        let cloud_url = link::cloud_url(cfg, env, build)?;
        runtime::spawn_release_updater(
            b,
            cloud_url,
            dir,
            http.clone(),
            exe_path.as_deref(),
            exit.clone(),
            &updater_slot,
        );
    }
    #[cfg(target_os = "linux")]
    if let (Some(b), Some(exe)) = (build, exe_path.as_deref()) {
        runtime::register_desktop_entries(b, exe, dir, env);
    }
    let settings = Arc::new(SettingsServer::new(
        dir.to_path_buf(),
        port,
        default_port,
        network_enabled,
        env.clone(),
        || SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        move |code| exit(code),
        keychain.clone(),
    ));
    settings.set_status_source(StatusSource {
        view: (*view).clone(),
        started: std::time::Instant::now(),
        checks: latest_checks,
        live,
    });
    axum::serve(
        listener,
        api::router(version.to_string(), settings).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    for c in connectors {
        c.stop(Duration::from_secs(10)).await;
    }
    Ok(())
}

/// What the cloud sees of this computer.
fn agent_view(cfg: &config::ConfigFile, tags: Vec<String>, version: &str, env: &Env) -> view::AgentView {
    view::AgentView {
        machine: link::machine_name(env),
        tags,
        projects: cfg.projects.iter().map(view::ProjectView::from).collect(),
        version: version.to_string(),
        lanes: config::lanes(cfg, env),
        chore_lane: cfg.machine.chore_lane,
        lane_sizes: lane_pool::LaneSizes::default(),
        paused: view::PauseState::default(),
    }
}

/// cloud.md section 5 / environments.md section 3.4: starts one outbound link to each cloud,
/// every project on it, and the shared backlog's auto-link beside them.
#[allow(clippy::too_many_arguments)]
fn link_clouds(
    cloud_urls: &[String],
    keychain: &Arc<dyn link::TokenStore>,
    http: &reqwest::Client,
    view: &Arc<view::AgentView>,
    exit: &Arc<view::Exit>,
    dir: &Path,
    version: &str,
    env: &Env,
) -> Vec<connector::Connector> {
    let connectors: Vec<_> = cloud_urls
        .iter()
        .map(|url| {
            connector::Connector::start(
                url,
                keychain.clone(),
                http.clone(),
                view.clone(),
                connector::Timing::default(),
                exit.clone(),
                dir,
            )
        })
        .collect();
    // installers/03-the-bundle-agent-bundle.md section 3.10: named once, after the connections above
    // start, so the log says whether this build is linked — the start check greps for it, and a
    // release build has no other way to tell.
    let backlog_url = backlog::url(env);
    tracing::info!(
        "{}",
        link::links_line(cloud_urls, &backlog_url, |url| matches!(keychain.get(url), Ok(Some(_))))
    );
    // installers/04-every-agent-on-the-shared-backlog-shared.md: once this computer already holds a
    // retaskable.com link, the shared backlog's device flow starts by itself too, opening its own
    // `/link` page. Fire-and-forget so a slow or unreachable store doesn't delay serving `/api/health`;
    // it isn't tried again this run (a failed or expired code waits for the next start).
    if cloud_urls.iter().any(|url| matches!(keychain.get(url), Ok(Some(_)))) {
        tokio::spawn(auto_link_backlog(
            keychain.clone(),
            http.clone(),
            env.clone(),
            version.to_string(),
        ));
    }
    connectors
}

/// Resolves and logs this run's settings: this computer's tags, the poll interval, the local
/// model and Cursor's models, and watches the local model if it's on. Returns the effective tags,
/// since [`serve`] needs them for the cloud view.
fn log_runtime_settings(
    cfg: &config::ConfigFile,
    machine: &store::MachineStore,
    engine_choice: &config::EngineChoiceConfig,
    lane_count: u32,
    env: &Env,
) -> Vec<String> {
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
    let poll_interval_sec = config::poll_interval_sec(cfg, env);
    let local_model = config::local_model(cfg, env);
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
    let cursor_models = config::cursor_models(cfg, env);
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
    tag_list
}

/// `retaskable link [production|staging|local]`, or `retaskable link backlog`
/// (installers/04-every-agent-on-the-shared-backlog-shared.md, `npm run link -- backlog`): the device
/// flow in the terminal. The exit code.
async fn link_command(
    args: &[String],
    cfg: &config::ConfigFile,
    dir: &Path,
    env: &Env,
    version: &str,
    build: Option<&release::Build>,
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
    // Section 3.8 "One environment": a release build is fixed to `build.json`'s cloud, so linking
    // to any other is refused rather than silently ignored.
    let url = match args {
        [] => link::cloud_url(cfg, env, build).map_err(|e| format!("{e:#}")),
        [name] => match build {
            Some(b) if b.environment.name() != name => Err(format!(
                "This release build is fixed to {}, so it refuses to link to {name}.",
                b.environment.name()
            )),
            _ => link::environment_url(name).map(String::from).ok_or_else(String::new),
        },
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
            auto_link_backlog(
                keychain.clone(),
                reqwest::Client::new(),
                env.clone(),
                version.to_string(),
            )
            .await;
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
    env: &Env,
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
async fn auto_link_backlog(keychain: Arc<dyn link::TokenStore>, http: reqwest::Client, env: Env, version: String) {
    let shown = |code: &link::DeviceCode| {
        println!("Approve the shared backlog: {}", code.verification_uri_complete);
        if let Err(e) = link::open_url(&code.verification_uri_complete) {
            tracing::warn!("couldn't open the shared backlog's link page: {e:#}");
        }
    };
    match backlog::auto_link(keychain, http, &env, &version, shown).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_with_retry_succeeds_once_the_port_frees_up() {
        let holder = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = holder.local_addr().unwrap();
        let retry =
            tokio::spawn(async move { bind_with_retry(addr, Duration::from_millis(20), Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(holder);
        let listener = retry.await.unwrap().expect("bind should succeed once the port is free");
        assert_eq!(listener.local_addr().unwrap(), addr);
    }

    #[tokio::test]
    async fn bind_with_retry_gives_up_once_the_timeout_passes() {
        let holder = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = holder.local_addr().unwrap();
        let err = bind_with_retry(addr, Duration::from_millis(10), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        drop(holder);
    }
}
