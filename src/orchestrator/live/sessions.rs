//! The sessions running right now (agent-settings.md section 4.5, "Live sessions"). A session
//! registers itself and gets a guard that removes its row on drop, so a panic or early return can't
//! leave a stale row. A row holds names and times only: never a prompt or transcript text.
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::engines::choose::Role;
use crate::engines::engine::{CursorOptions, EngineId, SessionResult};
use crate::orchestrator::fresh_start::{EngineLimits, LIMIT_PAUSE};

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

/// The model a session runs on: `model` (a Claude tier), or on Cursor its own model name ("auto" when none).
pub fn model_name(engine: EngineId, model: &str, cursor: Option<&CursorOptions>) -> String {
    if engine != EngineId::Cursor {
        return model.to_string();
    }
    cursor
        .and_then(|c| c.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "auto".to_string())
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
    limits: EngineLimits,
    /// The agent's `.retaskable` dir, where session ends add Cursor's and Codex's tokens (machine.json).
    machine_dir: Option<Arc<PathBuf>>,
    next: u64,
    rows: BTreeMap<u64, (SessionInfo, Instant, Stamp)>,
}

/// The list of running sessions, shared by the loops (writers) and the status handler (reader).
#[derive(Debug, Clone, Default)]
pub struct Sessions(Arc<Mutex<Inner>>);

tokio::task_local! {
    static CURRENT: Stamp;
    static LIMITS: EngineLimits;
    static MACHINE_DIR: Option<Arc<PathBuf>>;
    static PROJECT: String;
}

/// One writer at a time: load, change and save machine.json under this lock so concurrent sessions don't lose counts.
static MACHINE_WRITE: Mutex<()> = Mutex::new(());

/// A session of `engine` ended with `result`: when it ended at the engine's limit, that engine is limited until
/// the usage reading's reset, else for [`LIMIT_PAUSE`] (engines/03-choosing-the-engine.md section 3.5). A Cursor
/// or Codex session's tokens go to the day's total in machine.json, and Cursor's limit is stamped. A no-op when no
/// session is scoped, or the `engine-limit-records` flag is Off.
pub fn record_session_end(engine: EngineId, result: &SessionResult) {
    let _ = LIMITS.try_with(|limits| {
        if engine == EngineId::Claude
            && limits.week_cutoff_on()
            && let Some(usage) = &result.usage
        {
            limits.cut_off_for_week(usage);
        }
        let records = limits.records_any_role();
        if records && result.limit_hit {
            let wait = crate::engines::engine::resets_in(
                result.usage.as_ref(),
                crate::tiers::weekly_reading_on() || limits.week_cutoff_on(),
            )
            .unwrap_or(LIMIT_PAUSE);
            limits.hit(engine, Instant::now() + wait);
        }
        let _ = MACHINE_DIR.try_with(|dir| {
            if let Some(dir) = dir {
                record_machine(dir, engine, result, records);
            }
        });
    });
}

/// Adds the session's cost to its project's day, and (with `records`, the `engine-limit-records` flag On) its tokens
/// (Cursor, Codex) and Cursor's limit stamp, to machine.json; a failure is logged only.
fn record_machine(dir: &std::path::Path, engine: EngineId, result: &SessionResult, records: bool) {
    let tokens = result
        .tokens
        .filter(|_| records && matches!(engine, EngineId::Cursor | EngineId::Codex));
    let stamp = records && result.limit_hit && engine == EngineId::Cursor;
    let spend = result
        .cost_usd
        .filter(|c| *c > 0.0)
        .and_then(|c| PROJECT.try_with(|p| (p.clone(), c)).ok());
    if tokens.is_none() && !stamp && spend.is_none() {
        return;
    }
    let _lock = MACHINE_WRITE.lock().unwrap_or_else(PoisonError::into_inner);
    let done = (|| -> anyhow::Result<()> {
        let mut machine = crate::store::MachineStore::load(dir)?;
        if let Some(t) = tokens {
            if engine == EngineId::Cursor {
                machine.add_cursor_tokens(t.input, t.output)?;
            } else {
                machine.add_codex_tokens(t.input, t.output)?;
            }
        }
        if stamp {
            machine.stamp_cursor_limit_at()?;
        }
        if let Some((project, cost)) = spend {
            machine.add_spend(&project, cost)?;
        }
        Ok(())
    })();
    if let Err(e) = done {
        tracing::warn!("couldn't record the session's tokens in machine.json: {e:#}");
    }
}

/// Marks the current session (if one is scoped) as having just printed a line. A no-op otherwise.
pub fn touch() {
    let _ = CURRENT.try_with(|stamp| *stamp.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now());
}

impl Sessions {
    /// Sessions that end add Cursor's and Codex's tokens to machine.json in `dir`.
    pub fn with_machine_dir(self, dir: PathBuf) -> Self {
        self.lock().machine_dir = Some(Arc::new(dir));
        self
    }

    /// The engines' limits on this computer, shared with the driver.
    pub fn limits(&self) -> EngineLimits {
        self.lock().limits.clone()
    }

    /// Adds a row; it goes when the returned guard drops.
    pub fn register(&self, info: SessionInfo) -> SessionGuard {
        let now = Instant::now();
        let project = info.project.clone();
        let stamp: Stamp = Arc::new(Mutex::new(now));
        let mut inner = self.lock();
        let id = inner.next;
        inner.next += 1;
        inner.rows.insert(id, (info, now, stamp.clone()));
        SessionGuard {
            sessions: self.clone(),
            id,
            stamp,
            project,
        }
    }

    /// Lists `info` while `fut` runs, and updates its quiet time as the session prints; the row goes when
    /// `fut` ends, fails or is dropped.
    pub async fn track<F: Future>(&self, info: SessionInfo, fut: F) -> F::Output {
        self.register(info).scope(fut).await
    }

    /// Lists a session named for what `spec` runs, with no lane (an ask, a draft, a scout, a triage or a round); it goes when the guard drops.
    pub fn register_spec(
        &self,
        project: &str,
        item_id: &str,
        title: Option<&str>,
        role: Role,
        spec: &crate::engines::SessionSpec,
    ) -> SessionGuard {
        self.register(SessionInfo {
            project: project.into(),
            item_id: item_id.into(),
            title: title.map(String::from),
            role,
            lane: None,
            engine: spec.engine.unwrap_or_default(),
            model: spec.model.clone(),
        })
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

    /// The rows once at least one is listed; fails the test if none shows up.
    #[cfg(test)]
    pub async fn rows_when_running(&self) -> Vec<SessionRow> {
        for _ in 0..600 {
            let rows = self.snapshot();
            if !rows.is_empty() {
                return rows;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("no session was ever listed");
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
    project: String,
}

impl SessionGuard {
    /// Runs `fut` with this row as the current session, so the runner's [`touch`] updates it.
    pub async fn scope<F: Future>(&self, fut: F) -> F::Output {
        let limits = self.sessions.limits();
        let dir = self.sessions.lock().machine_dir.clone();
        let current = CURRENT.scope(self.stamp.clone(), fut);
        LIMITS
            .scope(
                limits,
                MACHINE_DIR.scope(dir, PROJECT.scope(self.project.clone(), current)),
            )
            .await
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

    fn ended(limit_hit: bool, usage: Option<serde_json::Value>) -> SessionResult {
        SessionResult {
            ok: !limit_hit,
            subtype: String::new(),
            structured: None,
            error: None,
            cost_usd: None,
            session_id: String::new(),
            limit_hit,
            signed_out: false,
            context_tokens: None,
            usage,
            tokens: None,
        }
    }

    async fn end_in_scope(s: &Sessions, engine: EngineId, result: SessionResult) {
        let mut i = info("a");
        i.role = Role::Planner;
        s.track(i, async { record_session_end(engine, &result) }).await;
    }

    #[tokio::test]
    async fn a_non_implementer_session_at_its_limit_limits_only_its_engine_for_30_minutes() {
        for engine in [EngineId::Cursor, EngineId::Pi, EngineId::Codex] {
            let s = Sessions::default();
            s.limits().record_any_role(true);
            end_in_scope(&s, engine, ended(true, None)).await;
            let now = Instant::now();
            for e in [EngineId::Claude, EngineId::Cursor, EngineId::Pi, EngineId::Codex] {
                assert_eq!(s.limits().is_limited(e, now), e == engine);
            }
            assert!(
                s.limits()
                    .is_limited(engine, now + LIMIT_PAUSE - std::time::Duration::from_secs(5))
            );
            assert!(
                !s.limits()
                    .is_limited(engine, now + LIMIT_PAUSE + std::time::Duration::from_secs(5))
            );
        }
    }

    #[tokio::test]
    async fn a_rejected_reading_sets_the_reset() {
        let s = Sessions::default();
        s.limits().record_any_role(true);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        let usage = serde_json::json!({"status": "rejected", "resetsAt": at});
        end_in_scope(&s, EngineId::Claude, ended(true, Some(usage))).await;
        let now = Instant::now();
        assert!(
            s.limits()
                .is_limited(EngineId::Claude, now + std::time::Duration::from_secs(590))
        );
        assert!(
            !s.limits()
                .is_limited(EngineId::Claude, now + std::time::Duration::from_secs(620))
        );
    }

    #[tokio::test]
    async fn a_non_implementer_claude_session_at_the_weekly_cutoff_limits_claude_to_the_weeks_reset() {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 86_400;
        let usage = |u: f64| serde_json::json!({"unifiedWindows": {"seven_day": {"utilization": u, "resetsAt": at}}});
        for (on, u, engine, limited) in [
            (true, 0.96, EngineId::Claude, true),
            (true, 0.90, EngineId::Claude, false),
            (true, 0.96, EngineId::Cursor, false),
            (false, 0.96, EngineId::Claude, false),
        ] {
            let s = Sessions::default();
            s.limits().set_week_cutoff(on);
            end_in_scope(&s, engine, ended(false, Some(usage(u)))).await;
            let t = Instant::now();
            let day = std::time::Duration::from_secs(80_000);
            assert_eq!(s.limits().is_limited(EngineId::Claude, t + day), limited);
            assert!(!s.limits().is_limited(EngineId::Claude, t + 2 * day));
        }
    }

    #[tokio::test]
    async fn a_session_that_did_not_hit_the_limit_or_ran_with_the_flag_off_records_nothing() {
        let s = Sessions::default();
        s.limits().record_any_role(true);
        end_in_scope(&s, EngineId::Cursor, ended(false, None)).await;
        assert!(!s.limits().is_limited(EngineId::Cursor, Instant::now()));
        let off = Sessions::default();
        end_in_scope(&off, EngineId::Cursor, ended(true, None)).await;
        assert!(!off.limits().is_limited(EngineId::Cursor, Instant::now()));
    }

    #[test]
    fn a_session_outside_any_scope_records_nothing_and_does_not_panic() {
        record_session_end(EngineId::Cursor, &ended(true, None));
    }

    fn with_tokens(limit_hit: bool, input: u64, output: u64) -> SessionResult {
        SessionResult {
            tokens: Some(crate::engines::engine::Tokens { input, output }),
            ..ended(limit_hit, None)
        }
    }

    fn machine_sessions(dir: &std::path::Path) -> Sessions {
        let s = Sessions::default().with_machine_dir(dir.to_path_buf());
        s.limits().record_any_role(true);
        s
    }

    fn machine(dir: &std::path::Path) -> crate::store::MachineStore {
        crate::store::MachineStore::load(dir).unwrap()
    }

    #[tokio::test]
    async fn cursor_tokens_add_up_and_a_limit_is_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let s = machine_sessions(dir.path());
        end_in_scope(&s, EngineId::Cursor, with_tokens(false, 10, 3)).await;
        assert_eq!(machine(dir.path()).cursor_today_tokens(), Some(13));
        assert!(machine(dir.path()).cursor_limit_at().is_none());
        end_in_scope(&s, EngineId::Cursor, with_tokens(true, 5, 2)).await;
        let m = machine(dir.path());
        assert_eq!(m.cursor_today_tokens(), Some(20));
        assert!(m.cursor_limit_at().is_some());
        assert!(m.codex_today_tokens().is_none());
    }

    #[tokio::test]
    async fn codex_tokens_go_to_codex_only() {
        let dir = tempfile::tempdir().unwrap();
        let s = machine_sessions(dir.path());
        end_in_scope(&s, EngineId::Codex, with_tokens(true, 7, 1)).await;
        let m = machine(dir.path());
        assert_eq!(m.codex_today_tokens(), Some(8));
        assert!(m.cursor_today_tokens().is_none() && m.cursor_limit_at().is_none());
    }

    #[tokio::test]
    async fn claude_pi_and_a_session_with_no_tokens_add_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let s = machine_sessions(dir.path());
        end_in_scope(&s, EngineId::Claude, with_tokens(false, 9, 9)).await;
        end_in_scope(&s, EngineId::Pi, with_tokens(false, 9, 9)).await;
        end_in_scope(&s, EngineId::Cursor, ended(false, None)).await;
        end_in_scope(&s, EngineId::Codex, ended(false, None)).await;
        let m = machine(dir.path());
        assert!(m.cursor_today_tokens().is_none() && m.codex_today_tokens().is_none());
    }

    fn costing(usd: Option<f64>) -> SessionResult {
        SessionResult {
            cost_usd: usd,
            ..ended(false, None)
        }
    }

    async fn end_for(s: &Sessions, project: &str, result: SessionResult) {
        let mut i = info("a");
        i.project = project.into();
        s.track(i, async { record_session_end(EngineId::Claude, &result) })
            .await;
    }

    #[tokio::test]
    async fn costs_add_up_per_project_for_today_with_or_without_the_limit_flag() {
        let dir = tempfile::tempdir().unwrap();
        for s in [
            machine_sessions(dir.path()),
            Sessions::default().with_machine_dir(dir.path().to_path_buf()),
        ] {
            end_for(&s, "a", costing(Some(0.25))).await;
            end_for(&s, "a", costing(Some(0.5))).await;
            end_for(&s, "b", costing(Some(1.0))).await;
            end_for(&s, "a", costing(None)).await;
        }
        let m = machine(dir.path());
        assert_eq!(
            (m.spend_today("a"), m.spend_today("b"), m.spend_today("c")),
            (1.5, 2.0, 0.0)
        );
    }

    #[tokio::test]
    async fn the_flag_off_adds_no_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let s = Sessions::default().with_machine_dir(dir.path().to_path_buf());
        end_in_scope(&s, EngineId::Cursor, with_tokens(false, 1, 1)).await;
        assert!(machine(dir.path()).cursor_today_tokens().is_none());
    }

    #[tokio::test]
    async fn sessions_ending_at_once_both_count() {
        let dir = tempfile::tempdir().unwrap();
        let s = machine_sessions(dir.path());
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let s = s.clone();
                tokio::spawn(async move { end_in_scope(&s, EngineId::Cursor, with_tokens(false, 1, 1)).await })
            })
            .collect();
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(machine(dir.path()).cursor_today_tokens(), Some(16));
    }

    #[tokio::test]
    async fn the_settings_pages_tags_survive_a_token_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("machine.json"), r#"{"network":{"enabled":false}}"#).unwrap();
        let s = machine_sessions(dir.path());
        end_in_scope(&s, EngineId::Codex, with_tokens(false, 1, 1)).await;
        let text = std::fs::read_to_string(dir.path().join("machine.json")).unwrap();
        assert!(text.contains("network") && text.contains("codexTokens"));
    }
}
