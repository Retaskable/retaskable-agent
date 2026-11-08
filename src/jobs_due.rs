//! Asking the store whether a daily job is due (`askDue`, agent/src/jobsDue.ts; daily-lighthouse.md section 2). The
//! store decides the day, the hour and whether today's result is in; the computer holds no schedule of its own. An
//! answer is not asked for again while one is pending, nor for a while after it: `DUE_MS` after a yes, `NOT_DUE_MS`
//! after a no, `UNREACHABLE_MS` after a failure, which is logged and counts as not due (fail closed).

use std::sync::{Arc, Mutex};

use crate::backlog::Client;

pub const DUE_MS: u64 = 5 * 60_000;
pub const NOT_DUE_MS: u64 = 5 * 60_000;
pub const UNREACHABLE_MS: u64 = 60_000;

#[derive(Default)]
struct State {
    pending: bool,
    /// No ask before this time (milliseconds on the clock).
    quiet_until: u64,
}

/// Asks about one job on one project.
pub struct DueAsker {
    client: Arc<Client>,
    repo_id: u64,
    token: Option<String>,
    job: String,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    state: Mutex<State>,
}

impl DueAsker {
    /// `now` is milliseconds from any fixed start; tests pass their own.
    pub fn new(
        client: Arc<Client>,
        repo_id: u64,
        token: Option<String>,
        job: &str,
        now: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            repo_id,
            token,
            job: job.to_string(),
            now: Box::new(now),
            state: Mutex::new(State::default()),
        }
    }

    /// The project's day when the store says the job is due, else `None` (not due, asked too recently, an ask
    /// still pending, or the store couldn't answer).
    pub async fn ask(&self) -> Option<String> {
        {
            let mut st = self.state.lock().unwrap();
            if st.pending || (self.now)() < st.quiet_until {
                return None;
            }
            st.pending = true;
        }
        let answer = self
            .client
            .jobs_due(self.repo_id, self.token.as_deref(), &[self.job.as_str()])
            .await;
        let (day, wait) = match answer {
            Ok(a) if a.due.iter().any(|j| j == &self.job) => (Some(a.day), DUE_MS),
            Ok(_) => (None, NOT_DUE_MS),
            Err(e) => {
                tracing::warn!("couldn't ask the store whether {} is due: {e}", self.job);
                (None, UNREACHABLE_MS)
            }
        };
        let mut st = self.state.lock().unwrap();
        st.pending = false;
        st.quiet_until = (self.now)() + wait;
        day
    }
}

/// Asks only for the project's day (`askDay`): `jobs_due` naming no job. Quiet periods as [`DueAsker`]'s: `DUE_MS`
/// after an answer, `UNREACHABLE_MS` after a failure, which is logged and gives no day (fail closed).
pub struct DayAsker {
    client: Arc<Client>,
    repo_id: u64,
    token: Option<String>,
    now: Box<dyn Fn() -> u64 + Send + Sync>,
    state: Mutex<State>,
}

impl DayAsker {
    pub fn new(
        client: Arc<Client>,
        repo_id: u64,
        token: Option<String>,
        now: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            repo_id,
            token,
            now: Box::new(now),
            state: Mutex::new(State::default()),
        }
    }

    /// The project's day, or `None` (asked too recently, an ask still pending, or the store couldn't answer).
    pub async fn ask(&self) -> Option<String> {
        {
            let mut st = self.state.lock().unwrap();
            if st.pending || (self.now)() < st.quiet_until {
                return None;
            }
            st.pending = true;
        }
        let answer = self.client.jobs_due(self.repo_id, self.token.as_deref(), &[]).await;
        let (day, wait) = match answer {
            Ok(a) => (Some(a.day), DUE_MS),
            Err(e) => {
                tracing::warn!("couldn't ask the store for the project's day: {e}");
                (None, UNREACHABLE_MS)
            }
        };
        let mut st = self.state.lock().unwrap();
        st.pending = false;
        st.quiet_until = (self.now)() + wait;
        day
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::tests::store;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn asker(base: String, clock: Arc<AtomicU64>) -> DueAsker {
        DueAsker::new(Arc::new(Client::new(base)), 7, Some("t".into()), "ideas", move || {
            clock.load(Ordering::SeqCst)
        })
    }

    #[tokio::test]
    async fn a_due_answer_returns_the_day_and_sends_the_job() {
        let (base, seen) = store(200, json!({ "value": { "day": "2026-11-03", "due": ["ideas"] } })).await;
        let a = asker(base, Arc::default());
        assert_eq!(a.ask().await.as_deref(), Some("2026-11-03"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["op"], "jobs_due");
        assert_eq!(seen[0]["args"], json!({ "jobs": ["ideas"] }));
    }

    #[tokio::test]
    async fn the_day_ask_names_no_job() {
        let (base, seen) = store(200, json!({ "value": { "day": "2026-11-03", "due": [] } })).await;
        let a = DayAsker::new(Arc::new(Client::new(base)), 7, Some("t".into()), || 0);
        assert_eq!(a.ask().await.as_deref(), Some("2026-11-03"));
        assert_eq!(seen.lock().unwrap()[0]["args"], json!({ "jobs": [] }));
        assert_eq!(a.ask().await, None, "quiet after an answer");
    }

    #[tokio::test]
    async fn not_due_returns_none() {
        let (base, _) = store(200, json!({ "value": { "day": "2026-11-03", "due": [] } })).await;
        assert_eq!(asker(base, Arc::default()).ask().await, None);
    }

    #[tokio::test]
    async fn quiet_periods_follow_a_yes_and_a_no() {
        for (due, wait) in [(json!(["ideas"]), DUE_MS), (json!([]), NOT_DUE_MS)] {
            let (base, seen) = store(200, json!({ "value": { "day": "d", "due": due } })).await;
            let clock = Arc::new(AtomicU64::new(1_000));
            let a = asker(base, clock.clone());
            a.ask().await;
            clock.store(1_000 + wait - 1, Ordering::SeqCst);
            assert_eq!(a.ask().await, None);
            assert_eq!(seen.lock().unwrap().len(), 1, "no second ask inside the quiet period");
            clock.store(1_000 + wait, Ordering::SeqCst);
            a.ask().await;
            assert_eq!(seen.lock().unwrap().len(), 2, "asks again after it");
        }
    }

    #[tokio::test]
    async fn an_error_is_not_due_and_waits_a_minute() {
        let (base, seen) = store(500, json!({ "error": "boom" })).await;
        let clock = Arc::new(AtomicU64::new(0));
        let a = asker(base, clock.clone());
        assert_eq!(a.ask().await, None);
        clock.store(UNREACHABLE_MS - 1, Ordering::SeqCst);
        a.ask().await;
        assert_eq!(seen.lock().unwrap().len(), 1);
        clock.store(UNREACHABLE_MS, Ordering::SeqCst);
        a.ask().await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_unreachable_store_is_not_due() {
        let a = asker("http://127.0.0.1:1".to_string(), Arc::default());
        assert_eq!(a.ask().await, None);
    }

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }

    #[tokio::test]
    async fn a_failure_is_logged() {
        let buf = Buf::default();
        let sub = tracing_subscriber::fmt().with_writer(buf.clone()).finish();
        let _g = tracing::subscriber::set_default(sub);
        // Other tests may have cached this callsite as disabled while no subscriber was set.
        let a = asker("http://127.0.0.1:1".to_string(), Arc::default());
        // Parallel tests can race the cached interest, so rebuild and retry.
        let mut log = String::new();
        for _ in 0..20 {
            tracing::callsite::rebuild_interest_cache();
            a.ask().await;
            log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
            if log.contains("whether ideas is due") {
                break;
            }
        }
        assert!(log.contains("whether ideas is due"), "{log}");
    }
}
