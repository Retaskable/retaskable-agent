//! An expert session's pending messages and its continuation note, kept in the store
//! (docs/specs/expert-queue-on-server.md section 3.2; Retaskable.com's `agent/src/expert/queueStore.ts`) so the site
//! can edit and cancel them with this computer offline. This computer takes the rows of the sessions it runs (the
//! first taker wins), beats while it holds them and reports each as read. With the store out of reach it carries on
//! from memory: a message it could not write keeps a `local-` id, and what it owes the store (rows it consumed, a
//! continuation note, a discard) is sent when it is back. There is no queue file: the engine's own session files
//! never come here, and nothing is read from disk.
//!
//! The expert session (`orchestrator::driver::experts`) calls `add`, `take`, `done` and `clear`, runs `beat` every
//! [`BEAT_MS`] and calls `pull` at start and with each beat (this agent hears no `facts_changed` yet, so the beat
//! stands in for it). Not called yet: `set_continued` (a session that continues a Claude Code session).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backlog::expert::{ExpertFrom, ExpertRow, QueueList};
use crate::backlog::{BacklogError, Client};

/// How often a computer holding rows tells the store it is alive (the store lets a claim go after 10 minutes
/// without).
pub const BEAT_MS: u64 = 60_000;
const LOCAL: &str = "local-";

/// Whether `id` is a message only this computer's memory knows.
pub fn is_local(id: &str) -> bool {
    id.starts_with(LOCAL)
}

/// A pending message as the session sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub id: String,
    pub text: String,
    /// When it was written, in milliseconds since the epoch.
    pub at: u64,
    pub picture_ids: Vec<String>,
}

impl From<&ExpertRow> for Queued {
    fn from(r: &ExpertRow) -> Self {
        Self {
            id: r.id.clone(),
            text: r.text.clone(),
            at: r.at,
            picture_ids: r.picture_ids.clone(),
        }
    }
}

/// The store's expert-queue ops, for one project. [`StoreQueue`] sends them to the store; tests use an in-process
/// fake.
#[allow(async_fn_in_trait)]
pub trait QueueOps {
    async fn add(&self, item: &str, text: &str, by: &str, pictures: &[String]) -> Result<String, BacklogError>;
    async fn take(&self, item: &str, ids: &[String], machine: &str) -> Result<Vec<ExpertRow>, BacklogError>;
    async fn done(&self, item: &str, ids: &[String]) -> Result<bool, BacklogError>;
    async fn clear(&self, item: &str) -> Result<(), BacklogError>;
    async fn continues_set(&self, item: &str, from: &ExpertFrom) -> Result<(), BacklogError>;
    async fn list(&self) -> Result<QueueList, BacklogError>;
}

/// The project's backlog store, as [`QueueOps`].
pub struct StoreQueue<'a> {
    pub client: &'a Client,
    pub repo_id: u64,
    pub token: Option<&'a str>,
}

impl QueueOps for StoreQueue<'_> {
    async fn add(&self, item: &str, text: &str, by: &str, pictures: &[String]) -> Result<String, BacklogError> {
        self.client
            .expert_queue_add(self.repo_id, self.token, item, text, by, pictures)
            .await
    }
    async fn take(&self, item: &str, ids: &[String], machine: &str) -> Result<Vec<ExpertRow>, BacklogError> {
        self.client
            .expert_queue_take(self.repo_id, self.token, item, Some(ids), machine)
            .await
    }
    async fn done(&self, item: &str, ids: &[String]) -> Result<bool, BacklogError> {
        self.client.expert_queue_done(self.repo_id, self.token, item, ids).await
    }
    async fn clear(&self, item: &str) -> Result<(), BacklogError> {
        self.client.expert_queue_clear(self.repo_id, self.token, item).await
    }
    async fn continues_set(&self, item: &str, from: &ExpertFrom) -> Result<(), BacklogError> {
        self.client
            .expert_continues_set(self.repo_id, self.token, item, from)
            .await
    }
    async fn list(&self) -> Result<QueueList, BacklogError> {
        self.client.expert_queue_list(self.repo_id, self.token, None).await
    }
}

/// [`StoreQueue`] that owns its client, for a queue the session keeps across passes.
pub struct OwnedQueue {
    pub client: std::sync::Arc<Client>,
    pub repo_id: u64,
    pub token: String,
}

impl OwnedQueue {
    fn store(&self) -> StoreQueue<'_> {
        StoreQueue {
            client: &self.client,
            repo_id: self.repo_id,
            token: Some(&self.token),
        }
    }
}

impl QueueOps for OwnedQueue {
    async fn add(&self, item: &str, text: &str, by: &str, pictures: &[String]) -> Result<String, BacklogError> {
        self.store().add(item, text, by, pictures).await
    }
    async fn take(&self, item: &str, ids: &[String], machine: &str) -> Result<Vec<ExpertRow>, BacklogError> {
        self.store().take(item, ids, machine).await
    }
    async fn done(&self, item: &str, ids: &[String]) -> Result<bool, BacklogError> {
        self.store().done(item, ids).await
    }
    async fn clear(&self, item: &str) -> Result<(), BacklogError> {
        self.store().clear(item).await
    }
    async fn continues_set(&self, item: &str, from: &ExpertFrom) -> Result<(), BacklogError> {
        self.store().continues_set(item, from).await
    }
    async fn list(&self) -> Result<QueueList, BacklogError> {
        self.store().list().await
    }
}

#[derive(Default)]
struct State {
    notes: BTreeMap<String, ExpertFrom>,
    /// Notes the store has not heard yet.
    unsent: BTreeSet<String>,
    /// Sessions the store has not heard were discarded.
    clearing: BTreeSet<String>,
    /// Rows this computer consumed whose `done` the store has not acknowledged: shown no more, sent when it can.
    owed: BTreeMap<String, BTreeSet<String>>,
    /// Rows this computer holds (taken), kept alive by a beat.
    held: BTreeMap<String, BTreeSet<String>>,
}

/// One computer's view of the expert queue.
pub struct ExpertQueue<S: QueueOps> {
    ops: S,
    machine: String,
    state: Mutex<State>,
    local_seq: Mutex<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl<S: QueueOps> ExpertQueue<S> {
    pub fn new(ops: S, machine: impl Into<String>) -> Self {
        Self {
            ops,
            machine: machine.into(),
            state: Mutex::default(),
            local_seq: Mutex::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether this computer holds rows, so a beat is due.
    pub fn holding(&self) -> bool {
        !self.state().held.is_empty()
    }

    /// Writes a message into the session's queue and returns it. With the store out of reach (and not `strict`) it
    /// is a local row, kept in memory only; with `strict` the error comes back.
    pub async fn add(
        &self,
        item: &str,
        text: &str,
        by: &str,
        pictures: &[String],
        strict: bool,
    ) -> Result<Queued, BacklogError> {
        let text: String = text.chars().take(4000).collect();
        match self.ops.add(item, &text, by, pictures).await {
            Ok(id) => Ok(Queued {
                id,
                text,
                at: now_ms(),
                picture_ids: pictures.to_vec(),
            }),
            Err(e) if strict => Err(e),
            Err(_) => {
                let mut n = self.local_seq.lock().unwrap_or_else(|p| p.into_inner());
                *n += 1;
                Ok(Queued {
                    id: format!("{LOCAL}{}-{n}", now_ms()),
                    text,
                    at: now_ms(),
                    picture_ids: pictures.to_vec(),
                })
            }
        }
    }

    /// Takes these queued messages for this computer's run. They come back with the text the store has now (the
    /// person may have edited one meanwhile); `None` when another computer took them or the person cancelled them.
    /// The store being out of reach counts as taken, so the run carries on from memory.
    pub async fn take(&self, item: &str, queued: &[Queued]) -> Option<Vec<Queued>> {
        let real: Vec<String> = queued
            .iter()
            .filter(|q| !is_local(&q.id))
            .map(|q| q.id.clone())
            .collect();
        if real.is_empty() {
            return Some(queued.to_vec());
        }
        let rows = match self.ops.take(item, &real, &self.machine).await {
            Ok(rows) => rows,
            Err(e) if e.is_conflict() => return None,
            Err(_) => return Some(queued.to_vec()),
        };
        self.state()
            .held
            .entry(item.to_string())
            .or_default()
            .extend(real.iter().cloned());
        let got: Vec<Queued> = queued
            .iter()
            .filter_map(|q| {
                if is_local(&q.id) {
                    return Some(q.clone());
                }
                rows.iter().find(|r| r.id == q.id).map(|r| Queued {
                    text: r.text.clone(),
                    ..q.clone()
                })
            })
            .collect();
        (!got.is_empty()).then_some(got)
    }

    /// The run has read these: they are shown no more and leave the store's queue, now or when it can be reached.
    pub async fn done(&self, item: &str, ids: &[String]) {
        let real: Vec<&String> = ids.iter().filter(|i| !is_local(i)).collect();
        if real.is_empty() {
            return;
        }
        {
            let mut s = self.state();
            release(&mut s, item, Some(&real));
            s.owed
                .entry(item.to_string())
                .or_default()
                .extend(real.into_iter().cloned());
        }
        self.flush().await;
    }

    /// The session ended or was discarded: its rows and note go from the store, now or when it can be reached.
    pub async fn clear(&self, item: &str) {
        {
            let mut s = self.state();
            release(&mut s, item, None);
            s.notes.remove(item);
            s.unsent.remove(item);
            s.owed.remove(item);
            s.clearing.insert(item.to_string());
        }
        self.flush().await;
    }

    /// The Claude Code session this one continues, when it does.
    pub fn continued_from(&self, item: &str) -> Option<ExpertFrom> {
        self.state().notes.get(item).cloned()
    }

    /// Notes the session continues `from`; whether the store has it (otherwise it is sent when reachable).
    pub async fn set_continued(&self, item: &str, from: ExpertFrom) -> bool {
        self.state().notes.insert(item.to_string(), from.clone());
        let sent = self.ops.continues_set(item, &from).await.is_ok();
        let mut s = self.state();
        if sent {
            s.unsent.remove(item);
        } else {
            s.unsent.insert(item.to_string());
        }
        sent
    }

    /// Sends what the store was owed, then reads every session's rows and notes: the rows by item, or `None` when
    /// the store can't be read. Rows this computer consumed are left out.
    pub async fn pull(&self) -> Option<BTreeMap<String, Vec<Queued>>> {
        self.flush().await;
        let list = self.ops.list().await.ok()?;
        let resend: Vec<(String, ExpertFrom)> = {
            let mut s = self.state();
            let pending: BTreeMap<String, ExpertFrom> = s
                .notes
                .iter()
                .filter(|(id, _)| s.unsent.contains(*id))
                .map(|(id, f)| (id.clone(), f.clone()))
                .collect();
            let mut notes: BTreeMap<String, ExpertFrom> = list
                .continues
                .iter()
                .filter(|(id, _)| !s.clearing.contains(*id))
                .map(|(id, f)| (id.clone(), f.clone()))
                .collect();
            notes.extend(pending.clone());
            s.notes = notes;
            pending
                .into_iter()
                .filter(|(id, _)| !list.continues.contains_key(id))
                .collect()
        };
        for (id, from) in resend {
            self.set_continued(&id, from).await;
        }
        let s = self.state();
        Some(
            list.sessions
                .iter()
                .map(|(id, rows)| {
                    let owed = s.owed.get(id);
                    let rows = rows
                        .iter()
                        .filter(|r| !owed.is_some_and(|o| o.contains(&r.id)))
                        .map(Queued::from)
                        .collect();
                    (id.clone(), rows)
                })
                .collect(),
        )
    }

    /// Takes again what this computer holds, which moves its claim's time on. A row another computer holds now is
    /// let go. Call it every [`BEAT_MS`] while [`holding`](Self::holding).
    pub async fn beat(&self) {
        let held: Vec<(String, Vec<String>)> = self
            .state()
            .held
            .iter()
            .map(|(i, ids)| (i.clone(), ids.iter().cloned().collect()))
            .collect();
        for (item, ids) in held {
            if let Err(e) = self.ops.take(&item, &ids, &self.machine).await
                && e.is_conflict()
            {
                release(&mut self.state(), &item, None);
            }
        }
    }

    /// Sends the discards and the `done`s owed; what fails stays owed. A row another computer took meanwhile is
    /// let go.
    async fn flush(&self) {
        let clearing: Vec<String> = self.state().clearing.iter().cloned().collect();
        for item in clearing {
            if self.ops.clear(&item).await.is_ok() {
                self.state().clearing.remove(&item);
            }
        }
        let owed: Vec<(String, Vec<String>)> = self
            .state()
            .owed
            .iter()
            .map(|(i, ids)| (i.clone(), ids.iter().cloned().collect()))
            .collect();
        for (item, ids) in owed {
            // Taken first: a row taken here while the store was out of reach was never claimed.
            match self.ops.take(&item, &ids, &self.machine).await {
                Ok(_) => {}
                Err(e) if e.is_conflict() => {}
                Err(_) => continue,
            }
            if self.ops.done(&item, &ids).await.is_err() {
                continue;
            }
            let mut s = self.state();
            if let Some(o) = s.owed.get_mut(&item) {
                for i in &ids {
                    o.remove(i);
                }
                if o.is_empty() {
                    s.owed.remove(&item);
                }
            }
        }
    }
}

fn release(s: &mut State, item: &str, ids: Option<&[&String]>) {
    if let Some(held) = s.held.get_mut(item) {
        match ids {
            Some(ids) => ids.iter().for_each(|i| {
                held.remove(*i);
            }),
            None => held.clear(),
        }
        if held.is_empty() {
            s.held.remove(item);
        }
    }
}

#[cfg(test)]
mod tests;
