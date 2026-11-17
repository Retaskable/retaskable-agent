//! The line-delimited JSON client the browser driver talks to its node process through: one request per line on
//! its stdin (`{"id", "method", "params"}`), one response per line on its stdout (`{"id", "result"}` or
//! `{"id", "error": {"kind", "message"}}`). Data only ever travels as JSON, never through a shell. The process
//! starts in a [`ProcessTree`] and dies with the last clone of the [`Rpc`] (Chromium goes with it).
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

use crate::process_tree::{KILL_LIMIT, ProcessTree};
use crate::sync::lock;

/// How long one call may take before it is given up on (a route that never loads, a hung page).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(180);
/// How much of the process's stderr is kept for an error message.
const STDERR_KEEP: usize = 2000;

/// An error answer, or the process ending before it answered.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    /// `modules`, `no_chrome` or `other` (the script's own kinds); `ended` and `timeout` are this side's.
    pub kind: String,
    pub message: String,
}

impl RpcError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        RpcError {
            kind: kind.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

type Answer = Result<Value, RpcError>;

#[derive(Default)]
struct Pending {
    waiting: HashMap<u64, oneshot::Sender<Answer>>,
    /// Why the process is gone, once its stdout ended.
    ended: Option<String>,
}

struct Inner {
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<Pending>>,
    next: AtomicU64,
    tree: Arc<ProcessTree>,
    /// Held so `kill_on_drop` reaps it.
    _child: Mutex<Child>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.tree.kill_and_wait(KILL_LIMIT);
    }
}

/// A started process answering requests. Clones share it; the process is killed when the last one drops.
#[derive(Clone)]
pub struct Rpc(Arc<Inner>);

impl Rpc {
    /// Starts `command` (stdin, stdout and stderr are set here) and its reader and writer tasks. Needs a Tokio runtime.
    pub fn spawn(mut command: tokio::process::Command) -> std::io::Result<Rpc> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn()?;
        let tree = ProcessTree::join(&child).map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        let (stdin, stdout, stderr) = match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(i), Some(o), Some(e)) => (i, o, e),
            _ => return Err(std::io::Error::other("the process has no pipes")),
        };
        let pending = Arc::new(Mutex::new(Pending::default()));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let said = Arc::new(Mutex::new(String::new()));
        let said_in = said.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut said = lock(&said_in);
                said.push_str(&line);
                said.push('\n');
                if said.len() > STDERR_KEEP {
                    let cut = said.len() - STDERR_KEEP;
                    let at = (cut..said.len())
                        .find(|i| said.is_char_boundary(*i))
                        .unwrap_or(said.len());
                    said.drain(..at);
                }
            }
        });
        let waiting = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = v.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                let answer = match v.get("error") {
                    Some(e) => Err(RpcError::new(
                        e.get("kind").and_then(Value::as_str).unwrap_or("other"),
                        e.get("message").and_then(Value::as_str).unwrap_or("unknown error"),
                    )),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                if let Some(s) = lock(&waiting).waiting.remove(&id) {
                    let _ = s.send(answer);
                }
            }
            let stderr_tail = lock(&said).trim().to_string();
            let why = if stderr_tail.is_empty() {
                "the browser process ended".to_string()
            } else {
                format!("the browser process ended: {stderr_tail}")
            };
            let mut p = lock(&waiting);
            for (_, s) in p.waiting.drain() {
                let _ = s.send(Err(RpcError::new("ended", why.clone())));
            }
            p.ended = Some(why);
        });
        Ok(Rpc(Arc::new(Inner {
            tx,
            pending,
            next: AtomicU64::new(1),
            tree,
            _child: Mutex::new(child),
        })))
    }

    fn line(&self, method: &str, params: &Value) -> (u64, String) {
        let id = self.0.next.fetch_add(1, Ordering::SeqCst);
        let mut line = json!({ "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        (id, line)
    }

    /// One request and its answer; an error answer, a dead process and [`CALL_TIMEOUT`] are all an [`RpcError`].
    pub async fn call(&self, method: &str, params: Value) -> Answer {
        self.call_within(method, params, CALL_TIMEOUT).await
    }

    pub async fn call_within(&self, method: &str, params: Value, limit: Duration) -> Answer {
        let (id, line) = self.line(method, &params);
        let (s, r) = oneshot::channel();
        {
            let mut p = lock(&self.0.pending);
            if let Some(why) = &p.ended {
                return Err(RpcError::new("ended", why.clone()));
            }
            p.waiting.insert(id, s);
        }
        if self.0.tx.send(line).is_err() {
            lock(&self.0.pending).waiting.remove(&id);
            return Err(RpcError::new("ended", "the browser process is not taking requests"));
        }
        match tokio::time::timeout(limit, r).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(RpcError::new("ended", "the browser process ended")),
            Err(_) => {
                lock(&self.0.pending).waiting.remove(&id);
                Err(RpcError::new(
                    "timeout",
                    format!("{method} took over {}s", limit.as_secs()),
                ))
            }
        }
    }

    /// A request nobody waits for (closing a page from a `Drop`); its answer is ignored.
    pub fn send_and_forget(&self, method: &str, params: Value) {
        let (_, line) = self.line(method, &params);
        let _ = self.0.tx.send(line);
    }
}
