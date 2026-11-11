//! The tray agent (tray/04-the-tray-agent.md sections 4.2 and 4.6), following `agent/src/trayAgent.ts`: reads the
//! running agent into a [`TrayInput`], computes the menu, icon and tooltip, draws them on a [`TrayView`] every 2
//! seconds and at once when a watched signal changes, and runs the action of the item as last shown when one is
//! chosen. Everything it does to the world is behind a small trait, so tests use fakes.
#[cfg(test)]
pub(crate) mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::menu::{Action, Menu, tray_menu};
use super::state::{BacklogWait, CloudInput, Exit, IconState, LinkState, TrayInput, icon_state, tooltip};
use crate::connector::{CloudHandle, CloudLink};
use crate::link::pending::{BacklogWaiting, Links};
use crate::restart::RestartState;
use crate::settings_server::SettingsServer;
use crate::keep_awake::KeepAwake;
use crate::start_at_login::StartAtLogin;
use crate::view::AgentView;

/// How often the menu is read again; it's also read at once on each change the agent announces.
pub const REFRESH: Duration = Duration::from_secs(2);
/// The reason a pause from the tray records.
const PAUSE_REASON: &str = "paused from the tray";

/// What the view draws: the whole menu, the icon's state and the tooltip.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub menu: Menu,
    pub icon: IconState,
    pub tooltip: String,
}

/// What draws the menu: the tray helper or the terminal menu. It is given the whole frame each time and sends
/// only what changed.
pub trait TrayView: Send + Sync {
    fn draw(&self, frame: &Frame) -> Result<()>;
    fn close(&self);
}

/// One cloud's connector, as the tray reads and acts on it.
pub trait CloudConn: Send + Sync {
    fn connected(&self) -> CloudLink;
    fn forget(&self) -> Result<()>;
}

impl CloudConn for CloudHandle {
    fn connected(&self) -> CloudLink {
        CloudHandle::connected(self)
    }
    fn forget(&self) -> Result<()> {
        CloudHandle::forget(self)
    }
}

/// The links in progress: a cloud's code waiting, the shared backlog's approval, and starting a flow (which opens
/// the link page itself).
pub trait LinkRegistry: Send + Sync {
    /// The user code and its page while `url`'s flow waits; `flowing` is true from the start of a flow, before its code.
    fn waiting(&self, url: &str) -> Option<(String, String)>;
    fn flowing(&self, url: &str) -> bool;
    fn start(&self, url: &str) -> bool;
    fn backlog(&self) -> Option<BacklogWaiting>;
    fn subscribe(&self) -> tokio::sync::watch::Receiver<u64>;
}

impl LinkRegistry for Links {
    fn waiting(&self, url: &str) -> Option<(String, String)> {
        Links::waiting(self, url).map(|w| (w.user_code, w.page_url))
    }
    fn flowing(&self, url: &str) -> bool {
        !self.start_allowed(url)
    }
    fn start(&self, url: &str) -> bool {
        Links::start(self, url)
    }
    fn backlog(&self) -> Option<BacklogWaiting> {
        Links::backlog(self)
    }
    fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        Links::subscribe(self)
    }
}

/// What the tray does on the computer: open a page or a file, show a notification.
pub trait Opener: Send + Sync {
    /// Only ever called with an `http(s)` URL.
    fn open_url(&self, url: &str) -> Result<()>;
    fn open_file(&self, file: &Path) -> Result<()>;
    fn notify(&self, text: &str);
}

/// The OS's own: the default browser and app, without a shell.
pub struct SystemOpener;

impl Opener for SystemOpener {
    fn open_url(&self, url: &str) -> Result<()> {
        crate::link::open_url(url)
    }

    /// Explorer's on Windows, the default text editor on macOS, `xdg-open`'s on Linux.
    fn open_file(&self, file: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        let mut cmd = std::process::Command::new("explorer.exe");
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = std::process::Command::new("open");
            c.arg("-t");
            c
        };
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd.spawn().with_context(|| format!("opening {}", file.display()))?;
        Ok(())
    }

    fn notify(&self, text: &str) {
        crate::notify::notify(text);
    }
}

/// How the agent stops.
pub trait Quit: Send + Sync {
    /// Quit now: the agent's shutdown path (sessions stopped, their items go back as on any crash).
    fn now(&self);
    /// Quit when idle: the restart's exit, with 0 instead of 75.
    fn exit(&self, code: i32);
}

pub trait Login: Send + Sync {
    fn is_on(&self) -> bool;
    fn set(&self, on: bool) -> Result<()>;
}

impl Login for StartAtLogin {
    fn is_on(&self) -> bool {
        StartAtLogin::is_on(self)
    }
    fn set(&self, on: bool) -> Result<()> {
        StartAtLogin::set(self, on)
    }
}

/// The computer's own settings page link, a new one-time token each call.
pub trait SettingsLink: Send + Sync {
    fn settings_link(&self) -> String;
}

impl SettingsLink for SettingsServer {
    fn settings_link(&self) -> String {
        SettingsServer::settings_link(self)
    }
}

/// One cloud the tray lists.
pub struct CloudSlot {
    pub name: String,
    /// No trailing slash.
    pub url: String,
    pub conn: Arc<dyn CloudConn>,
}

/// What the tray reads and acts on.
pub struct TrayDeps {
    pub view: Arc<AgentView>,
    pub clouds: Vec<CloudSlot>,
    pub links: Arc<dyn LinkRegistry>,
    /// Sessions running now.
    pub sessions: Arc<dyn Fn() -> usize + Send + Sync>,
    pub restart: Arc<RestartState>,
    pub opener: Arc<dyn Opener>,
    pub quit: Arc<dyn Quit>,
    pub settings: Arc<dyn SettingsLink>,
    /// Release builds only: Start at login, Open the log and Open Retaskable.
    pub login: Option<Arc<dyn Login>>,
    /// Keep this computer awake, where the OS has a way to hold it.
    pub keep_awake: Option<Arc<KeepAwake>>,
    pub log_file: Option<PathBuf>,
    pub windows: bool,
    /// Link the shared backlog's store again.
    pub link_backlog: Arc<dyn Fn() + Send + Sync>,
}

pub struct TrayAgent {
    deps: TrayDeps,
    view: Arc<dyn TrayView>,
    /// Each item's action as last shown, so a click does what the person saw.
    actions: Mutex<HashMap<String, Action>>,
    quitting: AtomicBool,
    exited: AtomicBool,
    backlog_notified: AtomicBool,
    /// Serialises reads and draws, so the last read is the last drawn.
    drawing: Mutex<()>,
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

impl TrayAgent {
    pub fn new(deps: TrayDeps, view: Arc<dyn TrayView>) -> Arc<Self> {
        Arc::new(Self {
            deps,
            view,
            actions: Mutex::default(),
            quitting: AtomicBool::new(false),
            exited: AtomicBool::new(false),
            backlog_notified: AtomicBool::new(false),
            drawing: Mutex::default(),
        })
    }

    fn cloud_input(&self, slot: &CloudSlot) -> CloudInput {
        let waiting = self.deps.links.waiting(&slot.url);
        let link = if let Some((code, _)) = &waiting {
            LinkState::Waiting {
                code: Some(code.clone()),
            }
        } else if self.deps.links.flowing(&slot.url) {
            LinkState::Waiting { code: None }
        } else {
            match slot.conn.connected() {
                CloudLink::NotLinked => LinkState::NotLinked,
                CloudLink::Offline => LinkState::Offline,
                CloudLink::Connected => LinkState::Connected,
                CloudLink::UnlinkedOnTheWebsite => LinkState::UnlinkedOnTheWebsite,
            }
        };
        CloudInput {
            name: slot.name.clone(),
            url: slot.url.clone(),
            host: host_of(&slot.url),
            link_page: waiting.map(|(_, page)| page),
            link,
        }
    }

    pub fn input(&self) -> TrayInput {
        let projects = &self.deps.view.projects;
        let step = self.deps.restart.step();
        let exit = if self.quitting.load(Ordering::SeqCst) {
            Exit::QuittingWhenIdle
        } else if step.is_some() {
            Exit::RestartingForNewCode
        } else {
            Exit::None
        };
        TrayInput {
            clouds: self.deps.clouds.iter().map(|c| self.cloud_input(c)).collect(),
            all_paused: projects.iter().all(|p| self.deps.view.pause_reason(&p.id).is_some()),
            sessions: (self.deps.sessions)(),
            has_project: !projects.is_empty(),
            exit,
            backlog: self.deps.links.backlog().map(|b| BacklogWait {
                host: host_of(&b.page_url),
                unlinked: b.label.starts_with("Unlinked on"),
                link_page: Some(b.page_url),
            }),
            is_release: self.deps.login.is_some(),
            start_at_login: self.deps.login.as_ref().is_some_and(|l| l.is_on()),
            keep_awake: self.deps.keep_awake.as_ref().map(|k| {
                k.tick();
                k.is_on()
            }),
            windows: self.deps.windows,
            machine: self.deps.view.machine.clone(),
            tags: self.deps.view.tags.get(),
        }
    }

    /// The agent as it is now, and the frame it draws; the frame's items are what a click then runs.
    fn read(&self) -> (TrayInput, Frame) {
        let input = self.input();
        let menu = tray_menu(&input);
        *self.actions.lock().unwrap() = menu
            .flatten()
            .into_iter()
            .filter(|i| i.enabled && !i.hidden)
            .filter_map(|i| Some((i.id.clone(), i.action.clone()?)))
            .collect();
        let frame = Frame {
            icon: icon_state(&input),
            tooltip: tooltip(&input),
            menu,
        };
        (input, frame)
    }

    /// The first frame, for a view that has to show something before it is ready.
    pub fn frame(&self) -> Frame {
        let _guard = self.drawing.lock().unwrap();
        self.read().1
    }

    /// Reads the agent again and draws it. Notifies once when the backlog starts waiting, and exits 0 when a
    /// Quit when idle finds nothing running.
    pub fn refresh(&self) {
        let _guard = self.drawing.lock().unwrap();
        let (input, frame) = self.read();
        if let Err(e) = self.view.draw(&frame) {
            tracing::warn!("tray: couldn't update the menu: {e:#}");
        }
        match &input.backlog {
            Some(b) if !self.backlog_notified.swap(true, Ordering::SeqCst) => self.deps.opener.notify(&format!(
                "Approve this computer on {}, open in your browser. It's in the tray's Waiting for you too.",
                b.host
            )),
            None => self.backlog_notified.store(false, Ordering::SeqCst),
            _ => {}
        }
        if self.quitting.load(Ordering::SeqCst)
            && input.sessions == 0
            && self.deps.restart.held().is_empty()
            && !self.exited.swap(true, Ordering::SeqCst)
        {
            tracing::info!("tray: nothing is running, quitting");
            self.view.close();
            self.deps.quit.exit(0);
        }
    }

    /// Draws every [`REFRESH`] and at once when pause, a restart or a link changes; ends when `stop` fires.
    pub async fn run(self: Arc<Self>, stop: tokio::sync::oneshot::Receiver<()>) {
        self.run_every(REFRESH, stop).await
    }

    pub async fn run_every(self: Arc<Self>, every: Duration, mut stop: tokio::sync::oneshot::Receiver<()>) {
        let mut paused = self.deps.view.paused.subscribe();
        let mut restart = self.deps.restart.subscribe();
        let mut links = self.deps.links.subscribe();
        let mut tick = tokio::time::interval(every);
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = tick.tick() => {}
                _ = paused.changed() => {}
                _ = restart.changed() => {}
                _ = links.changed() => {}
            }
            self.refresh();
        }
        self.view.close();
    }

    /// A chosen item, by id: runs its action as last shown; a greyed-out or hidden item does nothing.
    pub fn click(&self, id: &str) {
        let action = self.actions.lock().unwrap().get(id).cloned();
        if let Some(action) = action {
            if let Err(e) = self.act(&action) {
                tracing::warn!("tray: {action:?} failed: {e:#}");
            }
            self.refresh();
        }
    }

    /// A choice the view already resolved to its action (the helper tells which item, and checks it as last shown).
    pub fn choose(&self, action: &Action) {
        if let Err(e) = self.act(action) {
            tracing::warn!("tray: {action:?} failed: {e:#}");
        }
        self.refresh();
    }

    fn open(&self, url: &str) -> Result<()> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("not a web address: {url}");
        }
        self.deps.opener.open_url(url)
    }

    fn act(&self, action: &Action) -> Result<()> {
        match action {
            Action::Link(url) => {
                self.deps.links.start(url);
            }
            Action::Forget(url) => {
                let slot = self
                    .deps
                    .clouds
                    .iter()
                    .find(|c| &c.url == url)
                    .context("no such cloud")?;
                slot.conn.forget()?;
            }
            Action::LinkBacklog => (self.deps.link_backlog)(),
            Action::OpenUrl(url) => self.open(url)?,
            Action::Pause | Action::Resume => {
                let reason = (*action == Action::Pause).then(|| PAUSE_REASON.to_string());
                for p in &self.deps.view.projects {
                    self.deps.view.paused.set(&p.id, reason.clone());
                }
            }
            Action::Settings => self.open(&self.deps.settings.settings_link())?,
            Action::ToggleStartAtLogin => {
                if let Some(login) = &self.deps.login {
                    login.set(!login.is_on())?;
                }
            }
            Action::ToggleKeepAwake => {
                if let Some(k) = &self.deps.keep_awake {
                    k.set(!k.is_on())?;
                }
            }
            Action::OpenLog => {
                if let Some(file) = &self.deps.log_file {
                    self.deps.opener.open_file(file)?;
                }
            }
            Action::QuitWhenIdle => self.quitting.store(true, Ordering::SeqCst),
            Action::QuitNow => {
                self.view.close();
                self.deps.quit.now();
            }
        }
        Ok(())
    }
}
