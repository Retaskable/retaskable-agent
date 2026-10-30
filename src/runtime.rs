//! The long-lived pieces the binary starts: the restart handler, the background pollers and
//! watchers, and one project's startup. Split out of `main.rs`'s `serve` and `start_project`,
//! which only call these in order.
use crate::env::Env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

#[cfg(target_os = "linux")]
use crate::{app_menu, start_at_login};
use crate::{
    config, engines, lane_pool, lanes, phone, release, release_install, release_update, setup_check, store, telemetry,
    view,
};

/// Where the restart handler finds the updater. The updater's `on_ready` asks for a restart and
/// the restart installs what the updater has ready, so each needs the other and the updater can
/// only be filled in once the handler exists.
pub type UpdaterSlot = Arc<OnceLock<Arc<release_update::ReleaseUpdater>>>;

/// The exit shared by the settings page's own restart button, a newer build asking for one, and
/// Restart from retaskable.com (commands.rs): the one place code 75's exit installs whatever
/// `release_update` has ready first (installers/03-the-bundle-agent-bundle.md section 3.12 —
/// `ReleaseUpdater.installNow` runs "instead of relaunch"). When it doesn't install anything
/// (nothing ready, or the install failed), a release build falls back to the plain relaunch
/// section 3.8 describes; a checkout has none to fall back to, since nothing here supervises it
/// the way a release build's own exit does.
pub fn restart_handler(is_release: bool, exe_path: Option<PathBuf>, updater: UpdaterSlot) -> Arc<view::Exit> {
    Arc::new(move |code: i32| {
        let updater = updater.clone();
        let exe_path = exe_path.clone();
        // Restarting answers the request first: the process exit happens on another thread, a
        // moment later (agent/08-http-api.md: a restart finishes a merge first; this agent has
        // none in flight on its own HTTP thread, but the response still deserves to flush).
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let installed = code == 75 && updater.get().is_some_and(|updater| updater.install_now());
            if code == 75 && is_release && !installed {
                relaunch(exe_path.as_deref());
            }
            std::process::exit(code);
        });
    })
}

/// The plain relaunch a release build falls back to when no update was installed.
fn relaunch(exe_path: Option<&Path>) {
    let Some(exe) = exe_path else {
        tracing::error!("nothing to restart into (no executable path), so this copy stays down.");
        return;
    };
    let target = release_install::relaunch_target(exe);
    if let Err(e) = release_install::relaunch_with_same_args(&target) {
        tracing::error!("starting the next copy ({e}), so this copy stays down.");
    }
}

/// 04-architecture.md section 4.1: each lane's disk use, for the app's lanes panel. Walking a
/// lane's build output takes a while, so off the async threads and only every ten minutes.
pub fn spawn_lane_size_pollers(projects: &[config::ProjectConfig], sizes: &lane_pool::LaneSizes, lane_count: u32) {
    for project in projects.iter().filter(|p| !p.worktrees_dir.is_empty()) {
        let sizes = sizes.clone();
        let (id, worktrees_dir) = (project.id_or_derived(), PathBuf::from(&project.worktrees_dir));
        tokio::spawn(async move {
            let mut every = tokio::time::interval(lane_pool::SIZE_EVERY);
            loop {
                every.tick().await;
                let (sizes, id, worktrees_dir) = (sizes.clone(), id.clone(), worktrees_dir.clone());
                let _ = tokio::task::spawn_blocking(move || sizes.measure(&id, &worktrees_dir, lane_count)).await;
            }
        });
    }
}

/// first-run-check.md section 3.1: printed once the port is up, same as `agent/src/index.ts`.
/// Fire-and-forget so a slow or missing `gh`/`claude` doesn't delay serving `/api/health`. A
/// release build has no terminal for `println!` to reach, so it goes to `retaskable.log` instead.
/// The settings page's Now tab reads the latest result (agent-settings.md section 4.5), so it's
/// kept and run again every ten minutes, quietly.
pub fn spawn_setup_checks(is_release: bool, env: Env) -> Arc<Mutex<Option<setup_check::SetupChecks>>> {
    let latest = Arc::new(Mutex::new(None));
    let kept = latest.clone();
    tokio::spawn(async move {
        let checks = setup_check::run_setup_checks(&env).await;
        let report = setup_check::setup_check_report(&checks);
        if is_release {
            tracing::info!("{report}");
        } else {
            println!("{report}");
        }
        *kept.lock().unwrap() = Some(checks);
        loop {
            tokio::time::sleep(Duration::from_secs(600)).await;
            let checks = setup_check::run_setup_checks(&env).await;
            *kept.lock().unwrap() = Some(checks);
        }
    });
    latest
}

/// installers/03-the-bundle-agent-bundle.md section 3.11/3.12: a release build has no checkout
/// to move forward, so it asks its own environment's release feed for a newer build, downloads
/// and readies it, then asks for the same restart the settings page's own button does once
/// it's ready to install.
pub fn spawn_release_updater(
    build: &release::Build,
    cloud_url: String,
    dir: &Path,
    http: reqwest::Client,
    exe_path: Option<&Path>,
    exit: Arc<view::Exit>,
    slot: &UpdaterSlot,
) {
    let updater = Arc::new(release_update::ReleaseUpdater::new(release_update::ReleaseUpdateDeps {
        build: build.clone(),
        cloud_url,
        data_dir: dir.to_path_buf(),
        http,
        installer: exe_path.map(|p| release_install::installer_for(p) as Arc<dyn release_update::Installer>),
        on_ready: Some(Box::new(move |_ready| exit(75))),
        log: None,
        fetch: None,
    }));
    let _ = slot.set(updater.clone());
    tokio::spawn(release_update::watch(updater));
}

/// installers/03-the-bundle-agent-bundle.md sections 3.14 and 3.17: a Linux release build adds
/// itself to the login items on every start, and, when it's running as an AppImage, to the
/// desktop's app menu. macOS and Windows's own start-at-login are the desktop app's
/// (desktop-app.md) and the installer's, not this agent's (06-macos-and-linux.md section 6.1).
#[cfg(target_os = "linux")]
pub fn register_desktop_entries(build: &release::Build, exe: &Path, dir: &Path, env: &Env) {
    let home = env.get("HOME").unwrap_or_default();
    let appimage = env.get("APPIMAGE");
    let (file, text) = start_at_login::login_entry(
        build,
        env.get("XDG_CONFIG_HOME").as_deref(),
        appimage.as_deref(),
        &home,
        &exe.to_string_lossy(),
    );
    if let Err(e) = start_at_login::StartAtLogin::new(file, text).at_start(dir) {
        tracing::warn!("setting up start at login: {e:#}");
    }
    if let Some(entry) = app_menu::menu_entry(
        build,
        appimage.as_deref(),
        env.get("APPDIR").as_deref(),
        env.get("XDG_DATA_HOME").as_deref(),
        &home,
        exe,
    ) && let Err(e) = app_menu::add_to_app_menu(&entry, dir)
    {
        tracing::warn!("adding to the app menu: {e:#}");
    }
}

/// Starts one project: trims its state, opens its event log, keeps its lane pool sized, watches
/// for a phone if configured, and logs which engine runs each role. Nothing here is fatal — a
/// project not yet fully set up shouldn't keep the health check from coming up.
pub fn start_project(
    dir: &Path,
    lane_count: u32,
    engine_choice: &config::EngineChoiceConfig,
    project: &config::ProjectConfig,
    env: &Env,
) {
    let project_dir = dir.join("projects").join(project.id_or_derived());
    trim_state(&project_dir);
    // Opens this project's event log (agent-telemetry.md section 3.1), now that it's
    // configured with a real id: prunes files past KEEP_DAYS and reads the last day back.
    // Nothing calls `EventLog::add` yet — that needs the orchestrator loop, sessions and the
    // backlog client wired in first, none of which this agent runs yet.
    telemetry::EventLog::new().open(&project_dir.join("events"));
    ensure_lane_pool(project, lane_count);
    watch_phone(project, env);
    log_engines(project, engine_choice);
}

/// Trims the project's own state.json on startup (local-state-diet.md); not fatal, since a
/// broken file there shouldn't keep the health check from coming up.
fn trim_state(project_dir: &Path) {
    if let Err(e) = store::Store::load(project_dir) {
        tracing::warn!("loading {}: {e:#}", project_dir.join("state.json").display());
    }
}

/// Keeps this project's lane pool at lane_count worktrees (04-architecture.md section 4.1); not
/// fatal, since a project not yet pointed at a real repo and worktrees folder shouldn't keep the
/// health check from coming up either.
fn ensure_lane_pool(project: &config::ProjectConfig, lane_count: u32) {
    if project.repo_path.is_empty() || project.worktrees_dir.is_empty() {
        return;
    }
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

/// play-on-phone.md section 3: while this project has a `phone` section, watch for a
/// plugged-in, authorised Android phone. Only adb is looked up at start; re-finding it
/// when Setup is saved needs the settings server, not yet ported.
fn watch_phone(project: &config::ProjectConfig, env: &Env) {
    if project.phone.is_none() {
        return;
    }
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

/// Which program runs each kind of work in this project, once this computer's own
/// `engineChoice` (or, until it has one, this project's older `engines.default`/`roles`)
/// is resolved per role group. Not used to run a session yet: that needs the orchestrator
/// loop, still a later item.
fn log_engines(project: &config::ProjectConfig, engine_choice: &config::EngineChoiceConfig) {
    let legacy = project.engines.as_ref();
    tracing::info!(
        "{}: engines planner {}, implementer {}, review {}, drafter {}, chat {}, writer {}",
        project.id_or_derived(),
        engines::engine_for(engine_choice, legacy, engines::Role::Planner, None),
        engines::engine_for(engine_choice, legacy, engines::Role::Implementer, None),
        engines::engine_for(engine_choice, legacy, engines::Role::Scout, None),
        engines::engine_for(engine_choice, legacy, engines::Role::Drafter, None),
        engines::engine_for(engine_choice, legacy, engines::Role::Assistant, None),
        engines::engine_for(engine_choice, legacy, engines::Role::Writer, None),
    );
}
