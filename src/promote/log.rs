//! The deploy log on a shared backlog (last-local-state.md section 3.4, `StoreLogs` in `agent/src/storeLogs.ts`): the
//! store keeps one row a `log_add` of kind `deploy`, and a promote's row is kept on this computer first, in
//! `logs.pending.json` in the project's folder, so a restart or a store that can't be reached never loses one.
//!
//! It is its own file and not a key of `state.json` because `state.json` is the orchestrator's, written by its task
//! alone, and a promote runs on the deploy request loop's and the rollout's tasks: a second writer there would race
//! it. The file is the TypeScript agent's, row for row (`[{ kind, at, entry }]`, oldest first), so a computer that
//! changes agents sends what the other left.
//!
//! Lifecycle: [`DeployLog::add`] puts the row in the queue and on disk BEFORE anything is sent. [`DeployLog::flush_due`]
//! (every pass of the deploy request loop) and [`DeployLog::flush`] (right after a promote) send the queue in order.
//! A row leaves the queue, and the file, only once the store has answered it: taken, or refused as a bad request
//! (the row itself is wrong, so sending it again would not help; that one is dropped with a warning). Anything else,
//! an unreachable store most of all, keeps every row and waits [`RETRY_MS`] before the next try.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::backlog::Client;

/// The file's name in the project's folder.
pub const FILE: &str = "logs.pending.json";
/// How long to wait after a send the store didn't take (`RETRY_MS`).
pub const RETRY_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Queued {
    pub kind: String,
    pub at: u64,
    pub entry: Map<String, Value>,
}

/// One file's queue.
pub struct DeployLog {
    file: PathBuf,
    queue: Mutex<Vec<Queued>>,
    /// Held while rows are being sent: one sender at a time, so a row is never sent twice at once.
    sending: tokio::sync::Mutex<()>,
    /// Not before this time (ms since the epoch) does [`Self::flush_due`] try again.
    retry_at: AtomicU64,
}

/// A project's folder has one queue however many tasks hold it: a second would write the same file.
static SHARED: LazyLock<Mutex<HashMap<PathBuf, Arc<DeployLog>>>> = LazyLock::new(Mutex::default);

impl DeployLog {
    /// The queue kept in `file`: rows an earlier run left are read back. A file that can't be read as a queue is moved
    /// aside (`.bad`) with a warning, so what is in it is kept and never written over.
    pub fn open(file: &Path) -> Self {
        let queue = match std::fs::read_to_string(file) {
            Ok(text) => serde_json::from_str::<Vec<Queued>>(&text).unwrap_or_else(|e| {
                let aside = file.with_extension("json.bad");
                tracing::warn!(
                    "logs: {} isn't a queue ({e}); kept as {}",
                    file.display(),
                    aside.display()
                );
                let _ = std::fs::rename(file, &aside);
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Self {
            file: file.to_path_buf(),
            queue: Mutex::new(queue),
            sending: tokio::sync::Mutex::new(()),
            retry_at: AtomicU64::new(0),
        }
    }

    /// The one queue for `file`, shared by everything in this process that logs there.
    pub fn shared(file: &Path) -> Arc<Self> {
        SHARED
            .lock()
            .unwrap()
            .entry(file.to_path_buf())
            .or_insert_with(|| Arc::new(Self::open(file)))
            .clone()
    }

    /// Rows not sent yet, oldest first.
    pub fn pending(&self) -> Vec<Queued> {
        self.queue.lock().unwrap().clone()
    }

    /// The newest `n` deploys, newest first (`recentDeploys`): the store's, with this computer's own not sent yet ahead
    /// of them. A store that can't be asked leaves the unsent ones alone.
    pub async fn recent(&self, client: &Client, repo: u64, token: &str, n: usize) -> Vec<Value> {
        let rows = match client.logs_get(repo, Some(token), "deploy", n).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("logs: couldn't read the deploy log from the shared backlog: {e}");
                Vec::new()
            }
        };
        let seen: std::collections::HashSet<String> = rows.iter().map(Value::to_string).collect();
        let waiting = (self.pending().into_iter().rev())
            .filter(|q| q.kind == "deploy")
            .map(|q| Value::Object(q.entry))
            .filter(|v| !seen.contains(&v.to_string()));
        waiting.chain(rows).take(n).collect()
    }

    /// Queues a row and writes it to disk. Nothing is sent. A write that fails is logged and the row is still held
    /// here and sent as usual: only a restart before it is sent would lose it.
    pub fn add(&self, kind: &str, entry: Map<String, Value>) {
        let at = crate::program_status::now_ms();
        let mut queue = self.queue.lock().unwrap();
        queue.push(Queued {
            kind: kind.into(),
            at,
            entry,
        });
        self.persist(&queue);
    }

    fn persist(&self, queue: &[Queued]) {
        let wrote = if queue.is_empty() {
            match std::fs::remove_file(&self.file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        } else {
            let rows = serde_json::to_value(queue).unwrap_or_default();
            crate::fs_util::write_json_atomic(&self.file, &rows, false)
        };
        if let Err(e) = wrote {
            tracing::warn!("logs: couldn't keep the queue in {}: {e:#}", self.file.display());
        }
    }

    /// [`Self::flush`], unless a send the store didn't take was less than [`RETRY_MS`] ago.
    pub async fn flush_due(&self, client: &Client, repo: u64, token: &str, now_ms: u64) {
        if now_ms >= self.retry_at.load(Ordering::SeqCst) {
            self.flush(client, repo, token, now_ms).await;
        }
    }

    /// Sends the queue in order, now. Stops at the first row the store doesn't take (and tries again after
    /// [`RETRY_MS`]); a row it answers leaves the queue and the file.
    pub async fn flush(&self, client: &Client, repo: u64, token: &str, now_ms: u64) {
        let Ok(_one_at_a_time) = self.sending.try_lock() else {
            return;
        };
        loop {
            let Some(head) = self.queue.lock().unwrap().first().cloned() else {
                return;
            };
            match client.log_add(repo, Some(token), &head.kind, &head.entry).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("bad_request") => {
                    tracing::warn!("logs: the store refused a {} row, so it is dropped: {e}", head.kind);
                }
                Err(e) => {
                    tracing::warn!(
                        "logs: couldn't send a {} row to the store, trying again soon: {e}",
                        head.kind
                    );
                    self.retry_at.store(now_ms + RETRY_MS, Ordering::SeqCst);
                    return;
                }
            }
            let mut queue = self.queue.lock().unwrap();
            queue.remove(0);
            self.persist(&queue);
            drop(queue);
            self.prune(&head);
        }
    }

    /// A TestFlight row leaves this computer's own `testflight.log` (beside the queue's file) once the store has
    /// answered it, as the TypeScript's `prune`; a file left empty is removed. Anything else, or no such line, does
    /// nothing. The line is matched whole, as it was written.
    fn prune(&self, row: &Queued) {
        if row.kind != crate::testflight::LOG_KIND {
            return;
        }
        let file = self.file.with_file_name(crate::testflight::LOG_FILE);
        let Ok(text) = std::fs::read_to_string(&file) else {
            return;
        };
        let want = serde_json::to_string(&row.entry).unwrap_or_default();
        let mut lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        let Some(at) = lines.iter().position(|l| *l == want) else {
            return;
        };
        lines.remove(at);
        let done = if lines.is_empty() {
            std::fs::remove_file(&file)
        } else {
            std::fs::write(&file, lines.join("\n") + "\n")
        };
        if let Err(e) = done {
            tracing::warn!("logs: couldn't prune {}: {e}", file.display());
        }
    }
}
