//! Runs the tray (tray/04-the-tray-agent.md section 4.6): joins the async [`TrayHelper`] to the [`TrayAgent`]'s
//! synchronous view, draws and takes clicks in one task, and ends it when the agent shuts down or the helper
//! stops (then the agent carries on headless). Every way out tells the helper to exit and closes its input first.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use base64::Engine;
use tokio::sync::{mpsc, oneshot};

use super::agent::{Frame, TrayAgent, TrayDeps, TrayView};
use super::helper::{self, TrayEvent, TrayHelper};
use super::state::IconState;

/// How long a blocking shutdown waits for the helper to be told.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests;

/// The four icons, base64, in [`IconState`]'s order.
pub type Icons = [String; 4];

pub fn encode_icon(png_or_ico: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(png_or_ico)
}

fn slot(state: IconState) -> usize {
    match state {
        IconState::NotLinked => 0,
        IconState::Waiting => 1,
        IconState::Offline => 2,
        IconState::Connected => 3,
    }
}

enum Cmd {
    Draw(helper::Frame),
    Close,
}

/// The agent's view of the helper: frames go to the task that owns it.
pub struct HelperView {
    tx: mpsc::UnboundedSender<Cmd>,
    /// Ends (disconnects) when the task has told the helper to exit and closed its input.
    done: Mutex<std::sync::mpsc::Receiver<()>>,
    icons: Icons,
}

impl TrayView for HelperView {
    fn draw(&self, frame: &Frame) -> Result<()> {
        let frame = helper::Frame {
            menu: frame.menu.clone(),
            icon: self.icons[slot(frame.icon)].clone(),
            tooltip: frame.tooltip.clone(),
        };
        self.tx
            .send(Cmd::Draw(frame))
            .map_err(|_| anyhow!("the tray helper has stopped"))
    }

    fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }
}

impl HelperView {
    fn shutdown(&self) {
        self.close();
        let _ = self.done.lock().unwrap().recv_timeout(SHUTDOWN_WAIT);
    }
}

/// What [`HelperView::new`] leaves for the task.
pub struct Pending {
    rx: mpsc::UnboundedReceiver<Cmd>,
    _done: std::sync::mpsc::Sender<()>,
}

impl HelperView {
    pub fn new(icons: Icons) -> (Arc<Self>, Pending) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (done_tx, done) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                tx,
                done: Mutex::new(done),
                icons,
            }),
            Pending { rx, _done: done_tx },
        )
    }
}

/// A running tray, to shut down.
pub struct TrayHandle {
    view: Arc<HelperView>,
    stop: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl TrayHandle {
    /// Tells the helper to exit and closes its input, then ends the agent's refresh. Blocks briefly; may be called
    /// again.
    pub fn shutdown(&self) {
        if let Some(stop) = self.stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
        self.view.shutdown();
    }
}

/// Starts the helper binary with `agent`'s first frame, then runs the tray. `on_stopped` is called with why when the
/// helper stops later. An error means the tray didn't start.
pub async fn start(
    bin: &Path,
    deps: TrayDeps,
    icons: Icons,
    on_stopped: impl FnOnce(String) + Send + 'static,
) -> Result<TrayHandle, String> {
    let (view, pending) = HelperView::new(icons);
    let agent = TrayAgent::new(deps, view.clone());
    let first = agent.frame();
    let first = helper::Frame {
        icon: view.icons[slot(first.icon)].clone(),
        tooltip: first.tooltip,
        menu: first.menu,
    };
    let helper = TrayHelper::start(bin, &first).await?;
    Ok(run(helper, pending, agent, view, on_stopped))
}

/// Runs the tray over an already connected helper.
pub fn run(
    helper: TrayHelper,
    pending: Pending,
    agent: Arc<TrayAgent>,
    view: Arc<HelperView>,
    on_stopped: impl FnOnce(String) + Send + 'static,
) -> TrayHandle {
    let (stop_tx, stop_rx) = oneshot::channel();
    let stop = Arc::new(Mutex::new(Some(stop_tx)));
    tokio::spawn(agent.clone().run(stop_rx));
    let ended = stop.clone();
    tokio::spawn(async move {
        if let Some(why) = pump(helper, pending, &agent).await {
            if let Some(stop) = ended.lock().unwrap().take() {
                let _ = stop.send(());
            }
            on_stopped(why);
        }
    });
    TrayHandle { view, stop }
}

/// Draws what the agent sends and runs what the helper chooses. `Some(why)` when the helper stopped by itself;
/// `None` after it was told to exit.
async fn pump(mut helper: TrayHelper, mut pending: Pending, agent: &TrayAgent) -> Option<String> {
    let why = loop {
        tokio::select! {
            cmd = pending.rx.recv() => match cmd {
                Some(Cmd::Draw(frame)) => {
                    if let Err(e) = helper::TrayView::draw(&mut helper, &frame).await {
                        break e;
                    }
                }
                Some(Cmd::Close) | None => {
                    helper.exit().await;
                    return None; // dropping `pending` tells a waiting shutdown
                }
            },
            event = helper::TrayView::next(&mut helper) => match event {
                TrayEvent::Chose(action) => agent.choose(&action),
                TrayEvent::Stopped(why) => break why,
            },
        }
    };
    helper.exit().await;
    Some(why)
}
