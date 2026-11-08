//! The long-lived pieces the binary starts: the restart handler, the background pollers and
//! watchers, and one project's startup. Split out of `main.rs`'s `serve` and `start_project`,
//! which only call these in order.
use crate::env::Env;
use crate::runs::RunWait;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::telemetry::event::{Event, RestartAsked, RestartExit, RestartWaiting};
#[cfg(target_os = "linux")]
use crate::{app_menu, start_at_login};
use crate::{
    config, engines, lane_pool, lanes, phone, process_tree, release, release_install, release_update, restart,
    setup_check, shutdown, store, telemetry, view,
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
/// the way a release build's own exit does. Before any of that, it cuts every other session
/// (docs/specs/agent/07.4-stopping-and-restarting.md, `restart-cuts-sessions`): a restart doesn't
/// go through `main.rs`'s own graceful shutdown, so this is the only place that happens for it.
pub fn restart_handler(
    is_release: bool,
    exe_path: Option<PathBuf>,
    updater: UpdaterSlot,
    projects: Vec<config::ProjectConfig>,
    data_dir: PathBuf,
    state: Arc<restart::RestartState>,
    before_exit: Arc<dyn Fn() + Send + Sync>,
) -> Arc<view::Exit> {
    Arc::new(move |code: i32| {
        let state = state.clone();
        // The commands that know who asked have recorded it; any other caller (the settings page's button, a
        // release update) is "this computer". Asking again keeps the first, and starts nothing more.
        state.ask(restart::Why {
            at: telemetry::now_ms(),
            by: "this computer".into(),
            reason: "asked".into(),
        });
        if !state.start() {
            return;
        }
        let log = restart_log(&projects, &data_dir);
        if let Some(why) = state.why() {
            log(Event::RestartAsked(RestartAsked {
                reason: Some(why.reason),
                by: Some(why.by),
                waiting_on: Some(state.held()),
                ..Default::default()
            }));
        }
        let updater = updater.clone();
        let exe_path = exe_path.clone();
        let projects = projects.clone();
        let before_exit = before_exit.clone();
        // Restarting answers the request first: the process exit happens on another thread, a
        // moment later (agent/08-http-api.md: a restart finishes a merge first; this agent has
        // none in flight on its own HTTP thread, but the response still deserves to flush).
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let clock = restart::Clock::real();
            let flush = |max: Duration| std::thread::sleep(max.min(Duration::from_millis(200)));
            let seq = Sequence {
                state: &state,
                clock: &clock,
                log: &log,
                flush: &flush,
            };
            cut_every_session_then_exit(
                code,
                &projects,
                &updater,
                is_release,
                exe_path.as_deref(),
                &RunWait::real(),
                &seq,
                |code| {
                    before_exit();
                    std::process::exit(code)
                },
            );
        });
    })
}

/// What `restart_handler`'s exit thread does once its delay is up: cut every other session
/// first (docs/specs/agent/07.4-stopping-and-restarting.md, `restart-cuts-sessions`), then
/// install an update or relaunch, then exit. With the `runs` loop holding runs, it first waits for them to
/// end (docs/specs/pieces.md section 3.1, up to `wait.limit`), taking no new ones meanwhile. `exit` is a parameter so a test can see what's true
/// right before it's called, without actually ending the process.
#[allow(clippy::too_many_arguments)]
fn cut_every_session_then_exit(
    code: i32,
    projects: &[config::ProjectConfig],
    updater: &UpdaterSlot,
    is_release: bool,
    exe_path: Option<&Path>,
    wait: &RunWait,
    seq: &Sequence,
    exit: impl FnOnce(i32),
) {
    wait.wait();
    if seq.state.why().is_some() {
        // Lanes keep claiming and starting meanwhile: `stopping()` is only set by the cut below.
        seq.state.finish_first(seq.clock, &mut |on, ms| {
            let why = seq.state.why().expect("a pending restart has a why");
            (seq.log)(Event::RestartWaiting(RestartWaiting {
                reason: Some(why.reason),
                waiting_on: Some(on),
                ms: Some(ms as f64),
                ..Default::default()
            }));
        });
    }
    process_tree::cut_all();
    shutdown::stop_other_sessions(projects);
    let installed = code == 75 && updater.get().is_some_and(|updater| updater.install_now());
    if code == 75 && is_release && !installed {
        relaunch(exe_path);
    }
    if let Some(why) = seq.state.why() {
        (seq.flush)(restart::FLUSH);
        let drain = (seq.clock.now_ms)() - why.at;
        (seq.log)(Event::RestartExit(RestartExit {
            reason: Some(why.reason),
            by: Some(why.by),
            drain_ms: Some(drain.max(0) as f64),
            ..Default::default()
        }));
    }
    exit(code);
}

/// What the restart sequence needs besides the sessions: the pending restart, time, the event log and the cloud
/// link's flush (given the most it may take).
pub struct Sequence<'a> {
    pub state: &'a restart::RestartState,
    pub clock: &'a restart::Clock,
    pub log: &'a dyn Fn(Event),
    pub flush: &'a dyn Fn(Duration),
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

/// Writes a `restart_*` event to every project's event log (agent-telemetry.md section 3.1).
fn restart_log(projects: &[config::ProjectConfig], data_dir: &Path) -> impl Fn(Event) + use<> {
    let dirs: Vec<PathBuf> = projects
        .iter()
        .map(|p| data_dir.join("projects").join(p.id_or_derived()).join("events"))
        .collect();
    move |event| {
        for dir in &dirs {
            let mut log = telemetry::EventLog::new();
            log.open(dir);
            log.add(event.clone());
        }
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

/// The setup checks, plus the Docker line (agent-ci.md section 3.6) only with the `agent-ci` flag On.
async fn checks_with_docker(env: &Env, flags: &crate::flags::Flags) -> setup_check::SetupChecks {
    let mut checks = setup_check::run_setup_checks(env, flags.on("engine-setup-probes").await).await;
    if flags.on(crate::hello_settings::CI_FLAG).await {
        let image = std::fs::read_to_string(Path::new(".agent-ci").join("image")).ok();
        let image = image.as_deref().map(str::trim).filter(|i| !i.is_empty());
        let in_image = crate::agent_ci::docker::in_agent_image();
        checks.docker = Some(setup_check::docker_check("docker", in_image, image).await);
    }
    checks
}

/// first-run-check.md section 3.1: printed once the port is up, same as `agent/src/index.ts`.
/// Fire-and-forget so a slow or missing `gh`/`claude` doesn't delay serving `/api/health`. A
/// release build has no terminal for `println!` to reach, so it goes to `retaskable.log` instead.
/// The settings page's Now tab reads the latest result (agent-settings.md section 4.5), so it's
/// kept and run again every ten minutes, quietly.
pub fn spawn_setup_checks(
    is_release: bool,
    env: Env,
    flags: Arc<crate::flags::Flags>,
) -> Arc<Mutex<Option<setup_check::SetupChecks>>> {
    let latest = Arc::new(Mutex::new(None));
    let kept = latest.clone();
    tokio::spawn(async move {
        let checks = checks_with_docker(&env, &flags).await;
        let report = setup_check::setup_check_report(&checks, flags.on("engine-setup-probes").await);
        if is_release {
            tracing::info!("{report}");
        } else {
            println!("{report}");
        }
        *kept.lock().unwrap() = Some(checks);
        loop {
            tokio::time::sleep(Duration::from_secs(600)).await;
            let checks = checks_with_docker(&env, &flags).await;
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
    if let Some(usage) = crate::store::Store::load(&project_dir).ok().and_then(|s| s.usage()) {
        crate::tiers::note_usage(&usage);
    }
    // Opens this project's event log (agent-telemetry.md section 3.1), now that it's
    // configured with a real id: prunes files past KEEP_DAYS and reads the last day back.
    // Only the dry orchestrator loop (`orchestrator::driver`, RETASKABLE_ORCHESTRATOR) writes events
    // so far, through its own log; this one only prunes and reads back.
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use super::*;

    /// These share the process-wide stopping flag, so they run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn cuts_every_session_before_the_injected_exit_runs() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let seen_stopping = Mutex::new(None);
        let wait = idle_wait();
        cut_every_session_then_exit(
            0,
            &[],
            &UpdaterSlot::default(),
            false,
            None,
            &wait,
            &idle_seq(),
            |_code| {
                *seen_stopping.lock().unwrap() = Some(process_tree::stopping());
            },
        );
        assert_eq!(*seen_stopping.lock().unwrap(), Some(true));
        process_tree::reset_for_tests();
    }

    fn idle_seq() -> Sequence<'static> {
        seq_on(
            Box::leak(Box::default()),
            Box::leak(Box::new(fake_clock(|_| {}))),
            Box::leak(Box::default()),
        )
    }

    type Logged = Mutex<Vec<Event>>;

    fn seq_on(
        state: &'static restart::RestartState,
        clock: &'static restart::Clock,
        logged: &'static Logged,
    ) -> Sequence<'static> {
        let log: &'static dyn Fn(Event) = Box::leak(Box::new(move |e| logged.lock().unwrap().push(e)));
        let flush: &'static dyn Fn(Duration) = Box::leak(Box::new(move |d| {
            assert!(d <= restart::FLUSH);
            logged.lock().unwrap().push(Event::RestartBack(Default::default())); // marks the flush
        }));
        Sequence {
            state,
            clock,
            log,
            flush,
        }
    }

    /// A clock that moves only when slept, running `each` (with the time) at every sleep.
    fn fake_clock(each: impl Fn(i64) + Send + Sync + 'static) -> restart::Clock {
        let t = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let t2 = t.clone();
        restart::Clock {
            now_ms: Box::new(move || t.load(Ordering::SeqCst)),
            sleep: Box::new(move |d| each(t2.fetch_add(d.as_millis() as i64, Ordering::SeqCst) + d.as_millis() as i64)),
        }
    }

    fn why() -> restart::Why {
        restart::Why {
            at: 0,
            by: "ann".into(),
            reason: "asked".into(),
        }
    }

    fn kinds(logged: &Logged) -> Vec<&'static str> {
        logged
            .lock()
            .unwrap()
            .iter()
            .map(|e| match e {
                Event::RestartWaiting(_) => "waiting",
                Event::RestartExit(_) => "exit",
                Event::RestartBack(_) => "flush",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn a_restart_with_nothing_held_cuts_and_exits_75_at_once() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let (state, logged): (&'static restart::RestartState, &'static Logged) =
            (Box::leak(Box::default()), Box::leak(Box::default()));
        state.ask(why());
        let seq = seq_on(state, Box::leak(Box::new(fake_clock(|_| panic!("it slept")))), logged);
        let exited = Mutex::new(None);
        cut_every_session_then_exit(75, &[], &UpdaterSlot::default(), false, None, &idle_wait(), &seq, |c| {
            *exited.lock().unwrap() = Some((c, process_tree::stopping()));
        });
        assert_eq!(*exited.lock().unwrap(), Some((75, true)));
        assert_eq!(state.report().unwrap()["step"], "now");
        assert_eq!(kinds(logged), ["flush", "exit"]);
        process_tree::reset_for_tests();
    }

    #[test]
    fn a_held_guard_is_waited_on_without_cutting_until_released_and_lanes_keep_going() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let (state, logged): (&'static Arc<restart::RestartState>, &'static Logged) =
            (Box::leak(Box::default()), Box::leak(Box::default()));
        state.ask(why());
        let guard = Mutex::new(Some(state.hold("a merge")));
        let stopping_while_waiting = Arc::new(Mutex::new(Vec::new()));
        let (s2, w2) = (stopping_while_waiting.clone(), state.clone());
        let clock = fake_clock(move |now| {
            s2.lock()
                .unwrap()
                .push((process_tree::stopping(), w2.report().unwrap()["waitingOn"].clone()));
            if now >= 300 {
                guard.lock().unwrap().take();
            }
        });
        let seq = seq_on(state, Box::leak(Box::new(clock)), logged);
        cut_every_session_then_exit(
            75,
            &[],
            &UpdaterSlot::default(),
            false,
            None,
            &idle_wait(),
            &seq,
            |_| {},
        );
        let seen = stopping_while_waiting.lock().unwrap();
        assert!(
            seen.iter()
                .all(|(stopping, on)| !stopping && on == &serde_json::json!(["a merge"]))
        );
        assert_eq!(state.report().unwrap()["step"], "now");
        assert_eq!(kinds(logged), ["waiting", "flush", "exit"]);
        process_tree::reset_for_tests();
    }

    #[test]
    fn a_guard_never_released_is_given_up_on_after_two_minutes() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let (state, logged): (&'static Arc<restart::RestartState>, &'static Logged) =
            (Box::leak(Box::default()), Box::leak(Box::default()));
        state.ask(why());
        let _held = state.hold("a promote");
        let seq = seq_on(state, Box::leak(Box::new(fake_clock(|_| {}))), logged);
        let exited = Mutex::new(None);
        cut_every_session_then_exit(75, &[], &UpdaterSlot::default(), false, None, &idle_wait(), &seq, |c| {
            *exited.lock().unwrap() = Some(c)
        });
        assert_eq!(*exited.lock().unwrap(), Some(75));
        let waited = match &logged.lock().unwrap()[0] {
            Event::RestartWaiting(w) => w.ms,
            other => panic!("{other:?}"),
        };
        assert_eq!(waited, Some(restart::FINISH_FIRST.as_millis() as f64));
        process_tree::reset_for_tests();
    }

    fn idle_wait() -> RunWait {
        RunWait {
            limit: Duration::from_secs(5),
            held: Box::new(|| 0),
            begin: Box::new(|| {}),
        }
    }

    /// A wait whose held count is `n`, dropping to 0 once `end` is flipped; `begun` records the wind-down.
    fn wait_on(n: usize, limit: Duration, end: &Arc<AtomicBool>, begun: &Arc<AtomicBool>) -> RunWait {
        let (end, begun) = (end.clone(), begun.clone());
        let begin = begun.clone();
        RunWait {
            limit,
            held: Box::new(move || if end.load(Ordering::SeqCst) { 0 } else { n }),
            begin: Box::new(move || begin.store(true, Ordering::SeqCst)),
        }
    }

    #[test]
    fn with_no_live_runs_it_exits_at_once_without_winding_down() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let (end, begun) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let wait = wait_on(0, Duration::from_secs(60), &end, &begun);
        let started = Instant::now();
        let exited = AtomicBool::new(false);
        cut_every_session_then_exit(0, &[], &UpdaterSlot::default(), false, None, &wait, &idle_seq(), |_| {
            exited.store(true, Ordering::SeqCst)
        });
        assert!(exited.load(Ordering::SeqCst) && !begun.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_secs(5));
        process_tree::reset_for_tests();
    }

    #[test]
    fn it_waits_for_the_last_run_to_end_then_exits_before_the_limit_without_cutting_first() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let (end, begun) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let wait = wait_on(1, Duration::from_secs(60), &end, &begun);
        let ender = end.clone();
        let fake_run = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            ender.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let stopping_while_waiting = Mutex::new(None);
        cut_every_session_then_exit(0, &[], &UpdaterSlot::default(), false, None, &wait, &idle_seq(), |_| {
            *stopping_while_waiting.lock().unwrap() = Some(begun.load(Ordering::SeqCst));
        });
        fake_run.join().unwrap();
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(10),
            "{took:?}"
        );
        assert_eq!(
            *stopping_while_waiting.lock().unwrap(),
            Some(true),
            "no new runs while it waited"
        );
        process_tree::reset_for_tests();
    }

    #[test]
    fn a_run_that_never_ends_is_cut_once_the_limit_passes_and_then_it_exits() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        process_tree::reset_for_tests();
        let (end, begun) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let wait = wait_on(1, Duration::from_millis(300), &end, &begun);
        let started = Instant::now();
        let seen = Mutex::new(None);
        cut_every_session_then_exit(0, &[], &UpdaterSlot::default(), false, None, &wait, &idle_seq(), |_| {
            *seen.lock().unwrap() = Some(process_tree::stopping());
        });
        assert!(started.elapsed() >= Duration::from_millis(300));
        assert_eq!(*seen.lock().unwrap(), Some(true), "the trees were cut before exit");
        process_tree::reset_for_tests();
    }
}
