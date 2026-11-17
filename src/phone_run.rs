//! Play on phone from the site (docs/specs/play-on-phone.md sections 5 and 6, `agent/src/phone.ts`): the `phone_play`
//! and `phone_stop` commands and the phone view the snapshot carries. [`crate::phone`] finds adb and watches for
//! phones; this is the machine's one run of the button (one at a time, whichever project asked), the state it shows
//! (`phones`, adb's `status`, the last or current `run`) and the build and install itself: the project's fixed build
//! command, Capacitor's CLI and adb, never a model's, in the base checkout moved to `origin/<baseBranch>`.
//!
//! It holds the base checkout ([`crate::base_hold`]) for as long as it runs, so Play main, the baseline and the sync
//! leave it alone, and is refused while another has it. A phone that isn't plugged in with USB debugging allowed, a first
//! install not yet confirmed and a project with no `phone` section are refused with the reason, before anything starts.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::config::PhoneConfig;
use crate::phone::{self, Phone};
use crate::proc::{self, tail_lines};
use crate::process_tree::ProcessTree;
use crate::sync::lock;
use crate::telemetry::events::{iso_utc_millis, now_ms};
use crate::view::ProjectView;

/// Each step of a run is given this long (`STEP_TIMEOUT_MS`).
const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The last or current run, for the phone view (`PhoneRun`).
#[derive(Clone, Debug, PartialEq)]
struct Run {
    project: String,
    serial: String,
    model: String,
    /// `building`, `installing`, `done`, `failed`, `incompatible` or `stopped`.
    step: &'static str,
    message: Option<String>,
    at: String,
}

#[derive(Default)]
struct State {
    adb: Option<PathBuf>,
    devices: Vec<Phone>,
    status: Option<String>,
    last: Option<Run>,
    /// A run is building or installing, for this project, and may be cut.
    running: Option<Running>,
}

struct Running {
    project: String,
    stopped: Arc<AtomicBool>,
    tree: Arc<Mutex<Option<Arc<ProcessTree>>>>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static CHANGED: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

/// Signals when the phone view changed, so the snapshot is sent again.
pub fn subscribe() -> watch::Receiver<u64> {
    CHANGED.subscribe()
}

fn changed() {
    CHANGED.send_modify(|n| *n += 1);
}

/// The watcher's poll: the phones plugged in now, or why adb can't say (`status`).
pub fn publish_devices(adb: &Path, polled: &Result<Vec<Phone>, String>) {
    let (devices, status) = match polled {
        Ok(phones) => (phones.clone(), None),
        Err(e) if e == "adb devices timed out" => (Vec::new(), Some("adb devices timed out.".to_string())),
        Err(e) => (Vec::new(), Some(format!("adb devices failed: {e}"))),
    };
    let mut state = lock(&STATE);
    let was = (state.devices.clone(), state.status.clone());
    state.adb = Some(adb.to_path_buf());
    state.devices = devices;
    state.status = status;
    if was != (state.devices.clone(), state.status.clone()) {
        drop(state);
        changed();
    }
}

/// adb isn't on this computer: the phone view says so instead of listing phones.
pub fn adb_missing() {
    lock(&STATE).status = Some(phone::NO_ADB.to_string());
    changed();
}

/// Whether Play on phone is building or installing `project`'s app.
pub fn busy(project: &str) -> bool {
    lock(&STATE).running.as_ref().is_some_and(|r| r.project == project)
}

/// The snapshot's `phone` for a project with a `phone` section (`PhoneView`): the phones adb lists with whether each was
/// warned, adb's `status` when it can't list, the app's id, whether the app is work in progress, and the last or
/// current run if it was this project's.
pub fn view_json(project: &ProjectView, data_dir: &Path) -> Option<Value> {
    let cfg = project.phone.as_ref()?;
    let warned = phone::warned_serials(&data_dir.join(PHONES_FILE));
    let state = lock(&STATE);
    let mut view = json!({
        "phones": state.devices.iter().map(|d| json!({
            "serial": d.serial, "model": d.model, "state": d.state, "warned": warned.contains(&d.serial),
        })).collect::<Vec<_>>(),
        "appId": cfg.app_id,
        "wip": wip(cfg),
    });
    if let Some(status) = &state.status {
        view["status"] = json!(status);
    }
    if let Some(run) = state.last.as_ref().filter(|r| r.project == project.id) {
        view["run"] = json!({ "serial": run.serial, "model": run.model, "step": run.step, "at": run.at });
        if let Some(message) = &run.message {
            view["run"]["message"] = json!(message);
        }
    }
    Some(view)
}

/// `.retaskable/phones.json`, beside the other data: the serials that no longer need the install warning.
pub const PHONES_FILE: &str = "phones.json";

/// Whether the app's backlog item isn't done (the warning's "work in progress" line). The item's state isn't read here,
/// so an app that names an item is taken to be in progress (the TS agent reads the item's state from its store).
fn wip(cfg: &PhoneConfig) -> bool {
    cfg.item_id.as_deref().is_some_and(|id| !id.is_empty())
}

/// `phone_play { serial, confirmed?, uninstall? }`: builds the base branch's app and installs it on `serial`, one of the
/// phones adb lists as `device`. The first install to a phone needs `confirmed`, and uninstalling always does. `Ok` once
/// it has started; the run goes on in a thread and shows in the phone view.
pub fn play(
    project: &str,
    p: &ProjectView,
    data_dir: &Path,
    serial: &str,
    confirmed: bool,
    uninstall_first: bool,
) -> Result<(), String> {
    let Some(cfg) = p.phone.clone() else {
        return Err("Play on phone isn't set up.".to_string());
    };
    let file = data_dir.join(PHONES_FILE);
    let (phone, adb) = {
        let state = lock(&STATE);
        let phone = phone::check_play(
            &state.devices,
            &phone::warned_serials(&file),
            serial,
            confirmed,
            uninstall_first,
            &cfg.app_id,
            wip(&cfg),
        )?;
        if state.running.is_some() {
            return Err("Play on phone is already running.".to_string());
        }
        (phone, state.adb.clone())
    };
    let hold = crate::base_hold::hold_base(project, "Play on phone")?;
    if confirmed {
        let _ = phone::mark_warned(&file, &phone.serial);
    }
    let stopped = Arc::new(AtomicBool::new(false));
    let tree = Arc::new(Mutex::new(None));
    {
        let mut state = lock(&STATE);
        if state.running.is_some() {
            return Err("Play on phone is already running.".to_string());
        }
        state.running = Some(Running {
            project: project.to_string(),
            stopped: stopped.clone(),
            tree: tree.clone(),
        });
    }
    let at = iso_utc_millis(now_ms());
    let set = {
        let (project, phone, at) = (project.to_string(), phone.clone(), at.clone());
        move |step: &'static str, message: Option<String>| {
            lock(&STATE).last = Some(Run {
                project: project.clone(),
                serial: phone.serial.clone(),
                model: phone.model.clone(),
                step,
                message,
                at: at.clone(),
            });
            changed();
        }
    };
    set("building", None);
    let dir = match crate::lanes::prepare_base_checkout(&p.repo_path, &p.worktrees_dir, &p.base_branch) {
        Ok(dir) => dir,
        Err(e) => {
            let why = format!("Couldn't prepare the base checkout: {e:#}");
            lock(&STATE).running = None;
            set("failed", Some(why.clone()));
            return Err(why);
        }
    };
    std::thread::spawn(move || {
        let steps = Steps {
            dir,
            cfg,
            phone,
            adb,
            uninstall_first,
            stopped,
            tree,
        };
        let (step, message) = steps.run();
        lock(&STATE).running = None;
        set(step, message);
        // The base checkout is let go last: a sync put off meanwhile runs on the next poll.
        drop(hold);
    });
    Ok(())
}

/// `phone_stop`: ends the build or install. Not running is not an error.
pub fn stop() {
    let state = lock(&STATE);
    if let Some(running) = &state.running {
        running.stopped.store(true, Ordering::SeqCst);
        if let Some(tree) = lock(&running.tree).clone() {
            std::thread::spawn(move || {
                tree.kill_and_wait(crate::process_tree::KILL_LIMIT);
            });
        }
    }
}

struct Steps {
    dir: PathBuf,
    cfg: PhoneConfig,
    phone: Phone,
    adb: Option<PathBuf>,
    uninstall_first: bool,
    stopped: Arc<AtomicBool>,
    tree: Arc<Mutex<Option<Arc<ProcessTree>>>>,
}

impl Steps {
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// One fixed command in `cwd`, kept to its last 20 KB of output; `(code, output, timed out)`.
    fn step(&self, command: &str, cwd: &Path, env: &[(&str, Option<String>)]) -> (i32, String, bool) {
        let tree = self.tree.clone();
        let ran = proc::run_command_in_tree_env(command, cwd, STEP_TIMEOUT, env, move |t| {
            *lock(&tree) = Some(t);
        });
        *lock(&self.tree) = None;
        ran.unwrap_or_else(|e| (-1, format!("{e:#}"), false))
    }

    /// The run's end: its step and message, as the phone view shows them.
    fn run(&self) -> (&'static str, Option<String>) {
        let Some(adb) = &self.adb else {
            return ("failed", Some(phone::NO_ADB.to_string()));
        };
        let mut env: Vec<(&str, Option<String>)> = vec![
            // Without it, cmd.exe can't find gradlew.bat when Capacitor runs Gradle (spec section 5).
            ("NoDefaultCurrentDirectoryInExePath", None),
        ];
        // Gradle and Capacitor find the SDK through ANDROID_HOME: point it at the SDK adb came from.
        if std::env::var_os("ANDROID_HOME").is_none()
            && std::env::var_os("ANDROID_SDK_ROOT").is_none()
            && let Some(sdk) = adb
                .parent()
                .filter(|d| d.file_name().is_some_and(|f| f == "platform-tools"))
                .and_then(Path::parent)
        {
            env.push(("ANDROID_HOME", Some(sdk.to_string_lossy().into_owned())));
        }
        let serial = &self.phone.serial;
        if self.uninstall_first {
            let command = format!("{} -s {serial} uninstall {}", quoted(adb), self.cfg.app_id);
            let (code, output, _) = self.step(&command, &self.dir, &env);
            if self.stopped() {
                return ("stopped", None);
            }
            let already_gone = output.to_lowercase().contains("not installed") || output.contains("Unknown package");
            if code != 0 && !already_gone {
                return (
                    "failed",
                    Some(format!("Uninstalling failed:\n{}", tail_lines(&output, 10))),
                );
            }
        }
        if proc::needs_npm_install(&self.dir) {
            let install = proc::npm_install_command(&self.dir);
            let (code, output, timed_out) = self.step(install, &self.dir, &env);
            if self.stopped() {
                return ("stopped", None);
            }
            if code != 0 {
                return ("failed", Some(failure(install, code, timed_out, &output)));
            }
        }
        let build = &self.cfg.build_command;
        let (code, output, timed_out) = self.step(build, &self.dir, &env);
        if self.stopped() {
            return ("stopped", None);
        }
        if code != 0 {
            return ("failed", Some(failure(build, code, timed_out, &output)));
        }
        let Some(cli) = cap_cli(&self.dir, &self.cfg.app_dir) else {
            return (
                "failed",
                Some(format!(
                    "Capacitor's CLI isn't installed in {}/ (node_modules/@capacitor/cli).",
                    self.cfg.app_dir
                )),
            );
        };
        // Checked again: it may have been unplugged while building.
        if !lock(&STATE)
            .devices
            .iter()
            .any(|d| d.serial == *serial && phone::offerable(&d.state))
        {
            return (
                "failed",
                Some(format!("{} isn't connected any more.", self.phone.model)),
            );
        }
        if let Some(run) = lock(&STATE).last.as_mut() {
            run.step = "installing";
        }
        changed();
        let command = format!("node {} run android --target {serial} --no-sync", quoted(&cli));
        let (code, output, _) = self.step(&command, &self.dir.join(&self.cfg.app_dir), &env);
        if self.stopped() {
            return ("stopped", None);
        }
        if code != 0 {
            return install_failure(&output, &self.cfg.app_dir);
        }
        ("done", None)
    }
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

fn failure(command: &str, code: i32, timed_out: bool, output: &str) -> String {
    let how = if timed_out {
        "timed out".to_string()
    } else {
        format!("failed ({code})")
    };
    format!("`{command}` {how}:\n{}", tail_lines(output, 30))
}

/// Capacitor's CLI script in the checkout, run with node so no shell is needed to find it (`npx` is a `.cmd` on Windows).
pub fn cap_cli(dir: &Path, app_dir: &str) -> Option<PathBuf> {
    let rel: PathBuf = ["node_modules", "@capacitor", "cli", "bin", "capacitor"]
        .iter()
        .collect();
    [dir.join(app_dir).join(&rel), dir.join(&rel)]
        .into_iter()
        .find(|p| p.exists())
}

/// What a failed `cap run` means: the phone has the app under another key, Windows can't run Gradle's wrapper, Gradle can't
/// find the SDK, or else (`installFailure`).
pub fn install_failure(output: &str, app_dir: &str) -> (&'static str, Option<String>) {
    if output.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
        return (
            "incompatible",
            Some("The phone has the app signed with a different key.".to_string()),
        );
    }
    let tail = tail_lines(output, 30);
    let lower = output.to_lowercase();
    if lower.contains("'gradlew' is not recognized") {
        return (
            "failed",
            Some(format!(
                "Windows couldn't run Gradle's wrapper ({app_dir}/android/gradlew.bat). Check it exists, or run `npx cap sync android` in {app_dir}/ to add it.\n{tail}"
            )),
        );
    }
    if lower.contains("sdk location not found")
        || lower.contains("android_home")
        || lower.contains("android_sdk_root")
        || lower.contains("sdk.dir")
    {
        return (
            "failed",
            Some(format!(
                "Android's SDK wasn't found: set ANDROID_HOME, or open {app_dir}/ in Android Studio once.\n{tail}"
            )),
        );
    }
    ("failed", Some(format!("Installing failed:\n{tail}")))
}

#[cfg(test)]
mod tests;
