//! The local model's health check (docs/specs/agent/07-orchestrator-loop.md section 7.2's
//! "Is it up?"): while the computer's local model is turned on, `GET <baseUrl>/v1/models` every
//! minute, plain HTTP with a 5-second timeout. Picking a tier and actually routing a session onto
//! it while this check is failing (the "tiers act as if the fallback were off" part) is the
//! orchestrator's job; for now `watch` keeps a shared up flag in [`LocalModel`] (which also holds the
//! model's one slot) and logs on each up/down transition, the same pattern as `phone::watch`.
use crate::config::LocalModelConfig;
use crate::engines::engine::LocalTarget;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Checked every minute (section 7.2).
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Plain HTTP, a 5-second timeout (section 7.2).
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// One check of `GET <base_url>/v1/models`: up only on a successful status, never guessed from a
/// partial answer. A timeout, connection refusal or any other transport error counts as down.
pub async fn check_up(http: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    matches!(http.get(url).send().await, Ok(res) if res.status().is_success())
}

/// The computer's local model, shared by everything that may one day run a session on it: the config, whether
/// the last check found it up, and its one slot (section 7.2's "One local session at a time"). Clones share all of it.
#[derive(Clone, Debug)]
pub struct LocalModel {
    config: Arc<LocalModelConfig>,
    up: Arc<AtomicBool>,
    /// Whether a check has answered yet: `down()` stays false until one has.
    checked: Arc<AtomicBool>,
    /// Bumped when `down()` changes, so the connector can send a new snapshot.
    changes: Arc<tokio::sync::watch::Sender<u64>>,
    slot: Arc<Semaphore>,
    /// Sessions holding the slot (0 or 1) and sessions waiting for it.
    holding: Arc<AtomicU32>,
    waiting: Arc<AtomicU32>,
}

/// Holds the local model's one slot until it's dropped, however the holder's task ends (a panic included).
pub struct LocalSlot {
    _permit: OwnedSemaphorePermit,
    holding: Arc<AtomicU32>,
}

impl LocalSlot {
    fn new(permit: OwnedSemaphorePermit, holding: &Arc<AtomicU32>) -> Self {
        holding.fetch_add(1, Ordering::SeqCst);
        Self {
            _permit: permit,
            holding: holding.clone(),
        }
    }
}

impl Drop for LocalSlot {
    fn drop(&mut self) {
        self.holding.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Counts a session as waiting for the slot until dropped, even if its future is cancelled.
struct Waiting(Arc<AtomicU32>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A session running on the local model: where it runs, holding the model's one slot until dropped.
pub struct LocalRun {
    pub target: LocalTarget,
    _slot: LocalSlot,
}

impl LocalModel {
    /// Down until the first check answers.
    pub fn new(config: LocalModelConfig) -> Self {
        Self {
            config: Arc::new(config),
            up: Arc::new(AtomicBool::new(false)),
            checked: Arc::new(AtomicBool::new(false)),
            changes: Arc::new(tokio::sync::watch::channel(0).0),
            slot: Arc::new(Semaphore::new(1)),
            holding: Arc::new(AtomicU32::new(0)),
            waiting: Arc::new(AtomicU32::new(0)),
        }
    }

    /// A local model that is on and up, for tests of what runs on it.
    #[cfg(test)]
    pub fn up() -> Self {
        let local = Self::new(LocalModelConfig {
            enabled: true,
            base_url: "http://127.0.0.1:11436".into(),
            model: "tiel-coder-35b".into(),
        });
        local.up.store(true, Ordering::SeqCst);
        local
    }

    /// A local model that is turned off.
    pub fn off() -> Self {
        Self::new(LocalModelConfig {
            enabled: false,
            base_url: String::new(),
            model: String::new(),
        })
    }

    /// Whether it's turned on in the config.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// True only while it's turned on and the last check found it up.
    pub fn available(&self) -> bool {
        self.config.enabled && self.up.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn set_up(&self, up: bool) {
        self.up.store(up, Ordering::SeqCst);
    }

    /// True while it's turned on, a check has answered and the last one failed: what the snapshot reports as
    /// `usage.localDown`.
    pub fn down(&self) -> bool {
        self.config.enabled && self.checked.load(Ordering::SeqCst) && !self.up.load(Ordering::SeqCst)
    }

    /// Changes whenever `down()` does (as `watch` sees it), for a new snapshot.
    pub fn subscribe_down(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// Where a local session runs.
    pub fn target(&self) -> LocalTarget {
        LocalTarget {
            base_url: self.config.base_url.clone(),
            model: self.config.model.clone(),
        }
    }

    /// The slot if it's free right now, else `None` at once (scouts and chores, which never wait for it).
    pub fn try_take(&self) -> Option<LocalSlot> {
        self.slot
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|p| LocalSlot::new(p, &self.holding))
    }

    #[cfg(test)]
    pub fn mark_up_for_test(&self) {
        self.up.store(true, Ordering::SeqCst);
        self.checked.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn mark_down_for_test(&self) {
        self.up.store(false, Ordering::SeqCst);
        self.checked.store(true, Ordering::SeqCst);
    }

    /// Waits for the slot (the fallback, which runs on the local model instead of waiting for the limit).
    pub async fn take(&self) -> LocalSlot {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let _waiting = Waiting(self.waiting.clone());
        let permit = self
            .slot
            .clone()
            .acquire_owned()
            .await
            .expect("the local slot is never closed");
        LocalSlot::new(permit, &self.holding)
    }

    /// Sessions holding the slot right now: 0 or 1.
    pub fn holding(&self) -> u32 {
        self.holding.load(Ordering::SeqCst)
    }

    /// Sessions waiting in `take()` (so `claim()` and `run()` too) for the slot right now.
    pub fn waiting(&self) -> u32 {
        self.waiting.load(Ordering::SeqCst)
    }

    /// Whether a role on the tier ladder may be handed `local`: the model is
    /// available (section 7.1). A role's own slot waiting and engine checks come on top.
    pub fn ladder(&self) -> bool {
        self.available()
    }

    /// [`Self::ladder`] for a session on `engine`: only Claude Code runs on the local model.
    pub fn ladder_for(&self, on_claude: bool) -> bool {
        on_claude && self.ladder()
    }

    /// For a session whose tier came out `local`: waits for the one slot and gives it with the target to run on
    /// (the fallback rows wait, they don't skip the slot). `None` for any other tier.
    pub async fn claim(&self, tier: &str) -> Option<(LocalSlot, LocalTarget)> {
        if tier != crate::tiers::LOCAL {
            return None;
        }
        Some((self.take().await, self.target()))
    }

    /// The slot and target for a session whose tier is `local`, waiting for the slot (the fallback).
    pub async fn run(&self) -> LocalRun {
        LocalRun {
            _slot: self.take().await,
            target: self.target(),
        }
    }

    /// The same without waiting: `None` when the model is down or its slot is busy (extra capacity).
    pub fn try_run(&self) -> Option<LocalRun> {
        if !self.available() {
            return None;
        }
        Some(LocalRun {
            _slot: self.try_take()?,
            target: self.target(),
        })
    }

    /// One check, written to the up flag. Returns whether it changed.
    async fn check(&self, http: &reqwest::Client) -> bool {
        let up = check_up(http, &self.config.base_url).await;
        self.checked.store(true, Ordering::SeqCst);
        self.up.swap(up, Ordering::SeqCst) != up
    }

    /// Signals `subscribe_down` if `down()` is no longer `was`; gives the current value.
    fn signal_down(&self, was: bool) -> bool {
        let now = self.down();
        if now != was {
            self.changes.send_modify(|n| *n += 1);
        }
        now
    }
}

/// Checks the local model every `POLL_INTERVAL`, keeping its up flag and logging only when it goes up or
/// down. Runs until the process exits; called once per computer, not per project.
pub async fn watch(local: LocalModel) -> ! {
    let http = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .expect("building the local model HTTP client");
    let mut first = true;
    let mut was_down = false;
    loop {
        let changed = local.check(&http).await;
        was_down = local.signal_down(was_down);
        if changed || first {
            if local.up.load(Ordering::SeqCst) {
                tracing::info!("Local model: up at {}", local.config.base_url);
            } else {
                tracing::warn!("Local model: down");
            }
            first = false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

impl Default for LocalModel {
    fn default() -> Self {
        Self::off()
    }
}

impl PartialEq for LocalModel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.up, &other.up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};

    fn client() -> reqwest::Client {
        reqwest::Client::builder().timeout(CHECK_TIMEOUT).build().unwrap()
    }

    /// Starts a fake local model server answering `handler` on `GET /v1/models`, and gives its
    /// base URL.
    async fn fake_server(handler: axum::routing::MethodRouter) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/models", handler);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn check_up_is_true_only_on_a_successful_status() {
        let base = fake_server(get(|| async { "ok" })).await;
        assert!(check_up(&client(), &base).await);
    }

    #[tokio::test]
    async fn check_up_is_false_on_an_error_status() {
        let base = fake_server(get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "") })).await;
        assert!(!check_up(&client(), &base).await);
    }

    #[tokio::test]
    async fn check_up_is_false_when_nothing_is_listening() {
        // A bound-then-dropped listener's port refuses the connection outright.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!check_up(&client(), &format!("http://{addr}")).await);
    }

    #[tokio::test]
    async fn check_up_trims_a_trailing_slash_off_the_base_url() {
        let base = fake_server(get(|| async { "ok" })).await;
        assert!(check_up(&client(), &format!("{base}/")).await);
    }

    fn on(base_url: String) -> LocalModel {
        LocalModel::new(LocalModelConfig {
            enabled: true,
            base_url,
            model: "m".into(),
        })
    }

    #[test]
    fn down_signals_only_when_its_value_changes() {
        let local = LocalModel::new(LocalModelConfig {
            enabled: true,
            base_url: "http://localhost:1".into(),
            model: "m".into(),
        });
        let mut rx = local.subscribe_down();
        assert!(!local.down(), "not before the first check answers");
        local.mark_down_for_test();
        assert!(local.signal_down(false));
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        assert!(local.signal_down(true));
        assert!(!rx.has_changed().unwrap(), "unchanged");
        local.mark_up_for_test();
        assert!(!local.signal_down(true));
        assert!(rx.has_changed().unwrap());
    }

    #[tokio::test]
    async fn available_follows_the_checks_up_down_up() {
        let status = Arc::new(AtomicBool::new(true));
        let s = status.clone();
        let base = fake_server(get(move || {
            let ok = s.load(Ordering::SeqCst);
            async move {
                if ok {
                    axum::http::StatusCode::OK
                } else {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                }
            }
        }))
        .await;
        let local = on(base);
        assert!(!local.available(), "down before the first check");
        local.check(&client()).await;
        assert!(local.available());
        status.store(false, Ordering::SeqCst);
        local.check(&client()).await;
        assert!(!local.available());
        status.store(true, Ordering::SeqCst);
        local.check(&client()).await;
        assert!(local.available());
    }

    #[tokio::test]
    async fn off_is_never_available() {
        let base = fake_server(get(|| async { "ok" })).await;
        let mut local = on(base);
        local.config = Arc::new(LocalModelConfig {
            enabled: false,
            ..(*local.config).clone()
        });
        local.check(&client()).await;
        assert!(!local.available());
        assert!(!LocalModel::off().available());
    }

    #[test]
    fn target_gives_the_url_and_model() {
        let t = on("http://x:1".into()).target();
        assert_eq!((t.base_url.as_str(), t.model.as_str()), ("http://x:1", "m"));
    }

    #[tokio::test]
    async fn holding_goes_0_1_0_across_a_slots_life_and_after_a_panic() {
        let local = LocalModel::up();
        assert_eq!(local.holding(), 0);
        let slot = local.try_take().unwrap();
        assert_eq!(local.holding(), 1);
        drop(slot);
        assert_eq!(local.holding(), 0);
        let l = local.clone();
        let panicked = tokio::spawn(async move {
            let _slot = l.take().await;
            assert_eq!(l.holding(), 1);
            panic!("boom");
        })
        .await;
        assert!(panicked.is_err());
        assert_eq!(local.holding(), 0);
    }

    #[tokio::test]
    async fn waiting_counts_a_blocked_take_until_granted_or_cancelled() {
        let local = LocalModel::up();
        let held = local.try_take().unwrap();
        let l = local.clone();
        let granted = tokio::spawn(async move { l.take().await });
        tokio::task::yield_now().await;
        assert_eq!((local.holding(), local.waiting()), (1, 1));
        drop(held);
        let slot = granted.await.unwrap();
        assert_eq!((local.holding(), local.waiting()), (1, 0));
        let l = local.clone();
        let cancelled = tokio::spawn(async move { l.take().await });
        tokio::task::yield_now().await;
        assert_eq!(local.waiting(), 1);
        cancelled.abort();
        let _ = cancelled.await;
        assert_eq!(local.waiting(), 0);
        drop(slot);
        assert_eq!(local.holding(), 0);
    }

    #[tokio::test]
    async fn try_take_is_none_while_held_and_take_waits_for_the_drop() {
        let local = on("http://x".into());
        let guard = local.try_take().expect("free");
        assert!(local.try_take().is_none());
        let l = local.clone();
        let waiter = tokio::spawn(async move { l.take().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        drop(guard);
        let got = waiter.await.unwrap();
        assert!(local.try_take().is_none());
        drop(got);
        assert!(local.try_take().is_some());
    }

    #[tokio::test]
    async fn claim_gives_the_target_only_for_the_local_tier_and_a_second_claim_waits() {
        let local = LocalModel::up();
        assert!(local.claim("sonnet").await.is_none());
        assert!(local.claim("opus").await.is_none());
        let (slot, target) = local.claim("local").await.expect("the local tier claims");
        assert_eq!(target.model, "tiel-coder-35b");
        let l = local.clone();
        let second = tokio::spawn(async move { l.claim("local").await.is_some() });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!second.is_finished(), "a second local session waits for the slot");
        drop(slot);
        assert!(second.await.unwrap());
    }

    /// What each role does with the gate and the claim, as the drafter, triager, ask and second look call them.
    #[tokio::test]
    async fn each_role_is_local_only_in_its_rows_and_falls_back_with_local_down() {
        use crate::tiers::{LOCAL, OPUS, SONNET, implementer_tier, mid_tier, top_tier};
        let m = crate::config::ModelsConfig::default();
        let usage = |used: f64| serde_json::json!({"unifiedWindows": {"five_hour": {"utilization": used, "resetsAt": 4_000_000_000i64}}});
        let up = LocalModel::up();
        let down = LocalModel::off();
        // (used, mid/asks tier with local up, with it down, top tier up, down)
        for (used, mid_up, mid_down, top_up, top_down) in [
            (0.2, SONNET, SONNET, OPUS, OPUS),
            (0.9, LOCAL, SONNET, OPUS, OPUS),
            (0.99, LOCAL, SONNET, LOCAL, OPUS),
        ] {
            let u = usage(used);
            let tier = |l: &LocalModel| mid_tier(implementer_tier(Some(&u), 0, &m, l.ladder_for(true)));
            assert_eq!(tier(&up), mid_up, "{used}");
            assert_eq!(tier(&down), mid_down, "{used}");
            assert_eq!(top_tier(Some(&u), 0, &m, up.ladder_for(true)), top_up, "{used}");
            assert_eq!(top_tier(Some(&u), 0, &m, down.ladder_for(true)), top_down, "{used}");
            // Above the limit nothing is claimed, so the free slot stays free.
            if mid_up != LOCAL {
                assert!(up.claim(mid_up).await.is_none());
                assert!(up.try_take().is_some());
            }
        }
    }

    #[test]
    fn the_ladder_needs_the_model_up_and_a_claude_session() {
        assert!(LocalModel::up().ladder());
        assert!(LocalModel::up().ladder_for(true));
        assert!(!LocalModel::up().ladder_for(false), "not on Claude");
        assert!(!LocalModel::off().ladder_for(true), "local down");
        assert!(!LocalModel::off().ladder());
    }

    #[tokio::test]
    async fn a_panicking_holder_frees_the_slot() {
        let local = on("http://x".into());
        let l = local.clone();
        let r = tokio::spawn(async move {
            let _g = l.try_take().unwrap();
            panic!("boom");
        })
        .await;
        assert!(r.is_err());
        assert!(local.try_take().is_some());
    }
}
