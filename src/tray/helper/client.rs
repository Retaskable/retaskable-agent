//! The tray helper's JSON-lines protocol (tray/04-the-tray-agent.md section 4.6), following `agent/src/trayHelper.ts`:
//! `{"type":"ready"}` from the helper, then the whole menu as the first line, then `update-item` (one item, by its
//! seq id) and `update-menu` (icon and tooltip only), only for what changed, parents before children. Clicks come
//! back as `{"type":"clicked","__id":N}`.
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::tray::menu::{Action, Item, Menu, SEPARATOR};

/// How long the helper has to say it's ready.
const READY_IN: Duration = Duration::from_secs(10);
const INDENT: &str = "      ";

#[cfg(test)]
mod tests;

/// What the tray shows: the menu, the icon (base64) and the tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub menu: Menu,
    pub icon: String,
    pub tooltip: String,
}

/// What a person did, or that the tray went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    /// The action of the item chosen, as last shown.
    Chose(Action),
    /// The view stopped (the helper exited, the desktop's tray restarted, the terminal closed).
    Stopped(String),
}

/// Something that draws the menu: the tray helper, or the terminal menu.
#[allow(async_fn_in_trait)]
pub trait TrayView {
    /// Shows `frame`, sending only what changed since the last one.
    async fn draw(&mut self, frame: &Frame) -> Result<(), String>;
    /// The next choice, or that the view stopped (and again each time after).
    async fn next(&mut self) -> TrayEvent;
    /// Takes the view away.
    async fn stop(&mut self);
}

enum Incoming {
    Ready,
    Clicked(u64),
    Stopped,
}

/// An item as the helper is shown it: its fields, always all of them (anything left out reads as false).
struct Drawn {
    id: String,
    title: String,
    enabled: bool,
    checked: bool,
    hidden: bool,
    action: Option<Action>,
    children: Vec<Drawn>,
}

/// What the helper draws: an inline item's children become lines under it, indented, and hidden while it is.
fn drawn(items: &[Item]) -> Vec<Drawn> {
    let mut out = vec![];
    for item in items {
        let mut d = Drawn {
            id: item.id.clone(),
            title: item.text.clone(),
            enabled: item.enabled,
            checked: item.checked,
            hidden: item.hidden,
            action: item.action.clone(),
            children: vec![],
        };
        if item.inline {
            let under: Vec<Item> = item
                .children
                .iter()
                .map(|c| Item {
                    text: if c.text.is_empty() {
                        String::new()
                    } else {
                        format!("{INDENT}{}", c.text)
                    },
                    hidden: item.hidden || c.hidden,
                    ..c.clone()
                })
                .collect();
            out.push(d);
            out.extend(drawn(&under));
        } else {
            d.children = drawn(&item.children);
            out.push(d);
        }
    }
    out
}

fn fields(d: &Drawn) -> Value {
    json!({"title": d.title, "tooltip": "", "enabled": d.enabled, "checked": d.checked, "hidden": d.hidden})
}

fn wire(d: &Drawn, seq: u64) -> Value {
    let mut v = fields(d);
    v["items"] = json!([]);
    v["__id"] = json!(seq);
    v
}

/// What is shown for an item: its seq id, its fields (to tell a change) and what it does.
struct Shown {
    seq: u64,
    print: String,
    enabled: bool,
    hidden: bool,
    action: Option<Action>,
}

pub struct TrayHelper {
    write: Box<dyn AsyncWrite + Unpin + Send>,
    incoming: mpsc::UnboundedReceiver<Incoming>,
    shown: HashMap<String, Shown>,
    icon: String,
    tooltip: String,
    stopped: Option<String>,
    _child: Option<Child>,
}

impl TrayHelper {
    /// Starts the helper binary (no console window on Windows) and shows `frame`; fails if it can't start or
    /// isn't ready within 10 seconds.
    pub async fn start(bin: &Path, frame: &Frame) -> Result<Self, String> {
        let mut cmd = Command::new(bin);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("starting the tray helper {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().ok_or("the tray helper has no stdin")?;
        let stdout = child.stdout.take().ok_or("the tray helper has no stdout")?;
        let mut helper = Self::connect(stdout, stdin, frame, READY_IN).await?;
        helper._child = Some(child);
        Ok(helper)
    }

    /// Talks to a helper over `read` and `write`: waits for ready, then sends the whole menu.
    pub async fn connect(
        read: impl AsyncRead + Unpin + Send + 'static,
        write: impl AsyncWrite + Unpin + Send + 'static,
        frame: &Frame,
        ready_in: Duration,
    ) -> Result<Self, String> {
        let (tx, mut incoming) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue; // not one of its messages
                };
                let event = match (msg["type"].as_str(), msg["__id"].as_u64()) {
                    (Some("ready"), _) => Incoming::Ready,
                    (Some("clicked"), Some(id)) => Incoming::Clicked(id),
                    _ => continue,
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
            let _ = tx.send(Incoming::Stopped);
        });
        let wait = async {
            loop {
                match incoming.recv().await {
                    Some(Incoming::Ready) => return Ok(()),
                    Some(Incoming::Clicked(_)) => {}
                    Some(Incoming::Stopped) | None => {
                        return Err("the tray helper stopped before it was ready".to_string());
                    }
                }
            }
        };
        tokio::time::timeout(ready_in, wait)
            .await
            .map_err(|_| format!("the tray helper didn't start within {} seconds", ready_in.as_secs()))??;
        let mut helper = TrayHelper {
            write: Box::new(write),
            incoming,
            shown: HashMap::new(),
            icon: frame.icon.clone(),
            tooltip: frame.tooltip.clone(),
            stopped: None,
            _child: None,
        };
        helper.first(frame).await?;
        Ok(helper)
    }

    /// The first line is the whole menu.
    async fn first(&mut self, frame: &Frame) -> Result<(), String> {
        let mut next = 1;
        fn walk(items: &[Drawn], next: &mut u64, shown: &mut HashMap<String, Shown>) -> Vec<Value> {
            items
                .iter()
                .map(|d| {
                    let seq = *next;
                    *next += 1;
                    shown.insert(
                        d.id.clone(),
                        Shown {
                            seq,
                            print: fields(d).to_string(),
                            enabled: d.enabled,
                            hidden: d.hidden,
                            action: d.action.clone(),
                        },
                    );
                    let mut v = wire(d, seq);
                    v["items"] = json!(walk(&d.children, next, shown));
                    v
                })
                .collect()
        }
        let items = walk(&drawn(&frame.menu.items), &mut next, &mut self.shown);
        self.send(&json!({"icon": frame.icon, "title": "", "tooltip": frame.tooltip, "items": items, "isTemplateIcon": false}))
            .await
    }

    async fn send(&mut self, msg: &Value) -> Result<(), String> {
        let line = format!("{msg}\n");
        let sent = async {
            self.write.write_all(line.as_bytes()).await?;
            self.write.flush().await
        };
        sent.await.map_err(|e| format!("writing to the tray helper: {e}"))
    }

    /// Sends each item that changed, parents before their children, then the icon and tooltip if they changed.
    pub async fn update(&mut self, frame: &Frame) -> Result<(), String> {
        let mut lines = vec![];
        fn visit(items: &[Drawn], shown: &mut HashMap<String, Shown>, lines: &mut Vec<Value>) {
            for d in items {
                if let Some(s) = shown.get_mut(&d.id) {
                    s.action = d.action.clone();
                    let print = fields(d).to_string();
                    if d.title != SEPARATOR && s.print != print {
                        s.print = print;
                        s.enabled = d.enabled;
                        s.hidden = d.hidden;
                        lines.push(json!({"type": "update-item", "item": wire(d, s.seq), "seq_id": -1}));
                    }
                }
                visit(&d.children, shown, lines);
            }
        }
        visit(&drawn(&frame.menu.items), &mut self.shown, &mut lines);
        if frame.icon != self.icon || frame.tooltip != self.tooltip {
            self.icon = frame.icon.clone();
            self.tooltip = frame.tooltip.clone();
            lines.push(json!({"type": "update-menu", "menu": {
                "icon": frame.icon, "title": "", "tooltip": frame.tooltip, "items": [], "isTemplateIcon": false}}));
        }
        for line in &lines {
            self.send(line).await?;
        }
        Ok(())
    }

    /// Tells the helper to exit, then closes its stdin, which ends it too.
    pub async fn exit(&mut self) {
        if self.stopped.is_none() {
            let _ = self.send(&json!({"type": "exit"})).await;
            self.stopped = Some("the tray helper was told to exit".into());
        }
        let _ = self.write.shutdown().await;
    }
}

impl TrayView for TrayHelper {
    async fn draw(&mut self, frame: &Frame) -> Result<(), String> {
        self.update(frame).await
    }

    async fn next(&mut self) -> TrayEvent {
        loop {
            if let Some(why) = &self.stopped {
                return TrayEvent::Stopped(why.clone());
            }
            match self.incoming.recv().await {
                Some(Incoming::Clicked(id)) => {
                    // The item as last shown; a greyed-out or hidden one does nothing.
                    let hit = self.shown.values().find(|s| s.seq == id);
                    if let Some(Shown {
                        enabled: true,
                        hidden: false,
                        action: Some(a),
                        ..
                    }) = hit
                    {
                        return TrayEvent::Chose(a.clone());
                    }
                }
                Some(Incoming::Ready) => {}
                Some(Incoming::Stopped) | None => self.stopped = Some("the tray helper stopped".into()),
            }
        }
    }

    async fn stop(&mut self) {
        self.exit().await;
    }
}
