//! Starts the tray from `main` (tray/04-the-tray-agent.md sections 4.4 and 4.6 "Without a tray"): decides, spawns
//! the helper with the icons and runs the [`TrayAgent`](super::agent::TrayAgent). Whatever stops it from starting,
//! or stops it later, is said in the console and the agent carries on headless.
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use super::agent::{CloudSlot, Opener, Quit, SystemOpener, TrayAgent, TrayDeps};
use super::helper::helper_path;
use super::icons::{TrayState, load_tray_icon};
use super::mode::{TrayMode, tray_mode, use_tui};
use super::run::{Icons, encode_icon, start};
use super::term::StdTerm;
use super::tui::{TerminalMenu, TuiOpener};
use crate::connector::CloudHandle;
use crate::env::Env;
use crate::link::pending::Links;
use crate::release::Build;
use crate::restart::RestartState;
use crate::settings_server::SettingsServer;
use crate::tags::{OsKind, current_os};
use crate::terminal_menu::console::ConsoleCapture;
use crate::terminal_menu::when::{UiMode, no_tray_reason, ui_mode};
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
    /// Card flag `terminal-menu`.
    pub terminal_menu: bool,
    pub link_backlog: Arc<dyn Fn() + Send + Sync>,
    /// Keep this computer awake.
    pub keep_awake: Option<Arc<crate::keep_awake::KeepAwake>>,
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

/// Ends the running tray or terminal menu: the helper is told to exit and its input closed, or the terminal is
/// put back. May be called again; blocks briefly.
pub type Shutdown = Arc<dyn Fn() + Send + Sync>;

/// The terminal menu in place of the tray; `None` (the plain console) when the terminal can't be taken over.
fn start_tui(deps: TrayDeps, quit: Arc<dyn Quit>) -> Option<Shutdown> {
    let menu = match TerminalMenu::start(Arc::new(StdTerm::default()), ConsoleCapture::console(), quit) {
        Ok(menu) => menu,
        Err(e) => {
            tracing::warn!("Tray: running with the plain console, the terminal menu couldn't start: {e}.");
            return None;
        }
    };
    let agent = TrayAgent::new(deps, menu.clone());
    menu.attach(&agent);
    agent.refresh();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    tokio::spawn(agent.run(stopped));
    menu.restore_on_panic();
    menu.spawn_input();
    let stop = std::sync::Mutex::new(Some(stop));
    Some(Arc::new(move || {
        if let Some(stop) = stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
        menu.restore();
    }))
}

/// Whether the terminal menu is drawn. With the `terminal-menu` flag Off, `use_tui` decides as it always did; On, `ui_mode`.
fn ui(args: &[String], env: &Env, os: OsKind, has_helper: bool, interactive: bool, flag: bool) -> bool {
    if flag {
        ui_mode(args, env, std::env::consts::OS, has_helper, interactive) == UiMode::TerminalMenu
    } else {
        use_tui(args, env, os, interactive)
    }
}

/// The running tray or terminal menu, or `None` when the agent runs with the plain console (the console says why).
pub async fn boot(b: Boot<'_>) -> Option<Shutdown> {
    let os = current_os();
    let exe = b.exe.unwrap_or(Path::new(""));
    let terminals = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let helper = helper_path(b.build, exe, b.env, os);
    let tui = ui(b.args, b.env, os, helper.is_some(), terminals, b.terminal_menu);
    // Card flag `terminal-menu`: On says why there is no tray (once); Off keeps the console as it was.
    let reason = b
        .terminal_menu
        .then(|| no_tray_reason(b.args, b.env, std::env::consts::OS, helper.is_some()))
        .flatten();
    if let Some(why) = &reason {
        tracing::info!("{why}");
    }
    let bin = if tui {
        None
    } else {
        match tray_mode(b.args, b.env, os, helper) {
            TrayMode::Tray(bin) => Some(bin),
            TrayMode::Headless(why) => {
                if reason.is_none() {
                    tracing::info!("Tray: running headless, {why}.");
                }
                return None;
            }
        }
    };
    let (login, log_file) = login(b.build, b.exe, b.env);
    let quit: Arc<dyn Quit> = Arc::new(TrayQuit {
        now: b.quit_now,
        exit: b.exit,
    });
    let opener: Arc<dyn Opener> = if tui {
        Arc::new(TuiOpener)
    } else {
        Arc::new(SystemOpener)
    };
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
        opener,
        quit: quit.clone(),
        settings: b.settings,
        login,
        keep_awake: b.keep_awake,
        log_file,
        windows: cfg!(windows),
        link_backlog: b.link_backlog,
    };
    let Some(bin) = bin else {
        return start_tui(deps, quit);
    };
    match start(&bin, deps, icons(b.build, exe, os), b.on_stopped).await {
        Ok(handle) => Some(Arc::new(move || handle.shutdown())),
        Err(e) => {
            tracing::warn!("Tray: running headless, {e}.");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_needs_its_flag_to_use_ui_mode_and_a_terminal() {
        let args = vec!["--tui".to_string()];
        let env = Env::from_map(Default::default());
        let os = current_os();
        assert!(ui(&args, &env, os, true, true, true));
        assert!(!ui(&args, &env, os, true, false, true));
    }

    #[test]
    fn with_the_flag_off_use_tui_still_decides() {
        let args = vec!["--tui".to_string()];
        let env = Env::from_map(Default::default());
        assert!(ui(&args, &env, current_os(), true, true, false));
        assert!(!ui(&args, &env, current_os(), true, false, false));
    }
}
