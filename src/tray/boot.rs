//! Starts the tray from `main` (tray/04-the-tray-agent.md sections 4.4 and 4.6 "Without a tray"): decides, spawns
//! the helper with the icons and runs the [`TrayAgent`](super::agent::TrayAgent). Whatever stops it from starting,
//! or stops it later, is said in the console and the agent carries on headless.
use std::path::Path;
use std::sync::Arc;

use super::agent::{CloudSlot, SystemOpener, TrayDeps};
use super::helper::helper_path;
use super::icons::{TrayState, load_tray_icon};
use super::mode::{TrayMode, tray_mode};
use super::run::{Icons, TrayHandle, encode_icon, start};
use crate::connector::CloudHandle;
use crate::env::Env;
use crate::link::pending::Links;
use crate::release::Build;
use crate::restart::RestartState;
use crate::settings_server::SettingsServer;
use crate::tags::{OsKind, current_os};
use crate::view::{AgentView, Exit};

/// Everything the tray reads and acts on.
pub struct Boot<'a> {
    pub args: &'a [String],
    pub env: &'a Env,
    pub build: Option<&'a Build>,
    pub exe: Option<&'a Path>,
    pub view: Arc<AgentView>,
    /// Each cloud's URL and its connector's handle, in the same order.
    pub clouds: Vec<(String, CloudHandle)>,
    pub links: Links,
    pub settings: Arc<SettingsServer>,
    pub sessions: Arc<dyn Fn() -> usize + Send + Sync>,
    pub restart: Arc<RestartState>,
    /// Restart's exit, for Quit when idle.
    pub exit: Arc<Exit>,
    /// Quit now: the agent's own shutdown.
    pub quit_now: Arc<tokio::sync::Notify>,
    pub link_backlog: Arc<dyn Fn() + Send + Sync>,
    /// Said (with why) when the helper stops after it started.
    pub on_stopped: Box<dyn FnOnce(String) + Send>,
}

struct TrayQuit {
    now: Arc<tokio::sync::Notify>,
    exit: Arc<Exit>,
}

impl super::agent::Quit for TrayQuit {
    fn now(&self) {
        self.now.notify_one();
    }
    fn exit(&self, code: i32) {
        (self.exit)(code);
    }
}

/// The icons in [`super::run::Icons`]' order.
fn icons(build: Option<&Build>, exe: &Path, os: OsKind) -> Icons {
    [
        TrayState::NotLinked,
        TrayState::Waiting,
        TrayState::Offline,
        TrayState::Connected,
    ]
    .map(|s| encode_icon(&load_tray_icon(s, build, exe, os)))
}

type LoginParts = (Option<Arc<dyn super::agent::Login>>, Option<std::path::PathBuf>);

/// Start at login and Open the log are a Linux release build's (macOS and Windows have the installer's and the
/// desktop app's own).
#[cfg(target_os = "linux")]
fn login(build: Option<&Build>, exe: Option<&Path>, env: &Env) -> LoginParts {
    use crate::start_at_login::{StartAtLogin, login_entry};
    let (Some(build), Some(exe)) = (build, exe) else {
        return (None, None);
    };
    let home = env.get("HOME").unwrap_or_default();
    let (file, text) = login_entry(
        build,
        env.get("XDG_CONFIG_HOME").as_deref(),
        env.get("APPIMAGE").as_deref(),
        &home,
        &exe.to_string_lossy(),
    );
    let log = crate::release::logs_dir(build, env).join("retaskable.log");
    (Some(Arc::new(StartAtLogin::new(file, text))), Some(log))
}

#[cfg(not(target_os = "linux"))]
fn login(_: Option<&Build>, _: Option<&Path>, _: &Env) -> LoginParts {
    (None, None)
}

/// The running tray, or `None` when the agent runs headless (the console says why).
pub async fn boot(b: Boot<'_>) -> Option<TrayHandle> {
    let os = current_os();
    let exe = b.exe.unwrap_or(Path::new(""));
    let helper = helper_path(b.build, exe, b.env, os);
    let bin = match tray_mode(b.args, b.env, os, helper) {
        TrayMode::Tray(bin) => bin,
        TrayMode::Headless(why) => {
            tracing::info!("Tray: running headless, {why}.");
            return None;
        }
    };
    let (login, log_file) = login(b.build, b.exe, b.env);
    let deps = TrayDeps {
        view: b.view,
        clouds: b
            .clouds
            .into_iter()
            .map(|(url, conn)| CloudSlot {
                name: crate::link::environment_name(&url).into(),
                url,
                conn: Arc::new(conn),
            })
            .collect(),
        links: Arc::new(b.links),
        sessions: b.sessions,
        restart: b.restart,
        opener: Arc::new(SystemOpener),
        quit: Arc::new(TrayQuit {
            now: b.quit_now,
            exit: b.exit,
        }),
        settings: b.settings,
        login,
        log_file,
        windows: cfg!(windows),
        link_backlog: b.link_backlog,
    };
    match start(&bin, deps, icons(b.build, exe, os), b.on_stopped).await {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!("Tray: running headless, {e}.");
            None
        }
    }
}
