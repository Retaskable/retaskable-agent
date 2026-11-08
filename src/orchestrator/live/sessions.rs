//! The sessions running right now (agent-settings.md section 4.5, "Live sessions"). A session
//! registers itself and gets a guard that removes its row on drop, so a panic or early return can't
//! leave a stale row. A row holds names and times only: never a prompt or transcript text.
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::engines::choose::Role;
use crate::engines::engine::EngineId;

/// What a person registers a session as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub project: String,
    /// The item id, or the draft or ask id.
    pub item_id: String,
    pub title: Option<String>,
    pub role: Role,
    /// `None` for a session with no lane (an ask, a spec draft, a scout).
    pub lane: Option<u32>,
    pub engine: EngineId,
    pub model: String,
}

/// One running session, as of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub info: SessionInfo,
    pub started: Instant,
    /// When the session last printed a line (its start, before the first).
    pub last_output: Instant,
}

type Stamp = Arc<Mutex<Instant>>;

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    rows: BTreeMap<u64, (SessionInfo, Instant, Stamp)>,
}

/// The list of running sessions, shared by the loops (writers) and the status handler (reader).
#[derive(Debug, Clone, Default)]
pub struct Sessions(Arc<Mutex<Inner>>);

tokio::task_local! {
    static CURRENT: Stamp;
}

/// Marks the current session (if one is scoped) as having just printed a line. A no-op otherwise.
pub fn touch() {
    let _ = CURRENT.try_with(|stamp| *stamp.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now());
}

impl Sessions {
    /// Adds a row; it goes when the returned guard drops.
    pub fn register(&self, info: SessionInfo) -> SessionGuard {
        let now = Instant::now();
        let stamp: Stamp = Arc::new(Mutex::new(now));
        let mut inner = self.lock();
        let id = inner.next;
        inner.next += 1;
        inner.rows.insert(id, (info, now, stamp.clone()));
        SessionGuard {
            sessions: self.clone(),
            id,
            stamp,
        }
    }

    /// The rows, in the order the sessions started.
    pub fn snapshot(&self) -> Vec<SessionRow> {
        self.lock()
            .rows
            .values()
            .map(|(info, started, stamp)| SessionRow {
                info: info.clone(),
                started: *started,
                last_output: *stamp.lock().unwrap_or_else(PoisonError::into_inner),
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Keeps a session's row listed; dropping it removes the row.
#[derive(Debug)]
pub struct SessionGuard {
    sessions: Sessions,
    id: u64,
    stamp: Stamp,
}

impl SessionGuard {
    /// Runs `fut` with this row as the current session, so the runner's [`touch`] updates it.
    pub async fn scope<F: Future>(&self, fut: F) -> F::Output {
        CURRENT.scope(self.stamp.clone(), fut).await
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions.lock().rows.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(item: &str) -> SessionInfo {
        SessionInfo {
            project: "p".into(),
            item_id: item.into(),
            title: Some("T".into()),
            role: Role::Implementer,
            lane: Some(1),
            engine: EngineId::default(),
            model: "m".into(),
        }
    }

    #[test]
    fn a_registered_session_shows_and_drops_with_its_guard() {
        let s = Sessions::default();
        let a = s.register(info("a"));
        let b = s.register(info("b"));
        let rows = s.snapshot();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].info, info("a"));
        assert_eq!(rows[1].info.item_id, "b");
        drop(a);
        assert_eq!(s.snapshot().len(), 1);
        drop(b);
        assert!(s.snapshot().is_empty());
    }

    #[test]
    fn a_panic_does_not_leave_a_row() {
        let s = Sessions::default();
        let s2 = s.clone();
        let r = std::panic::catch_unwind(move || {
            let _g = s2.register(info("a"));
            panic!("boom");
        });
        assert!(r.is_err());
        assert!(s.snapshot().is_empty());
    }

    #[tokio::test]
    async fn touch_updates_only_the_scoped_session() {
        let s = Sessions::default();
        let a = s.register(info("a"));
        let _b = s.register(info("b"));
        touch(); // unset: a no-op
        let before = s.snapshot();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        a.scope(async { touch() }).await;
        let after = s.snapshot();
        assert!(after[0].last_output > before[0].last_output);
        assert_eq!(after[1].last_output, before[1].last_output);
    }
}
