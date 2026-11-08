//! The local model's health check (docs/specs/agent/07-orchestrator-loop.md section 7.2's
//! "Is it up?"): while the computer's local model is turned on, `GET <baseUrl>/v1/models` every
//! minute, plain HTTP with a 5-second timeout. Picking a tier and actually routing a session onto
//! it while this check is failing (the "tiers act as if the fallback were off" part) is the
//! orchestrator's job; for now `watch` keeps a shared up flag in [`LocalModel`] (which also holds the
//! model's one slot) and logs on each up/down transition, the same pattern as `phone::watch`.
use crate::config::LocalModelConfig;
use crate::engines::engine::LocalTarget;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
#[derive(Clone)]
pub struct LocalModel {
    config: Arc<LocalModelConfig>,
    up: Arc<AtomicBool>,
    slot: Arc<Semaphore>,
}

/// Holds the local model's one slot until it's dropped, however the holder's task ends (a panic included).
pub struct LocalSlot(#[allow(dead_code)] OwnedSemaphorePermit);

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
            slot: Arc::new(Semaphore::new(1)),
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

    /// True only while it's turned on and the last check found it up.
    pub fn available(&self) -> bool {
        self.config.enabled && self.up.load(Ordering::SeqCst)
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
        self.slot.clone().try_acquire_owned().ok().map(LocalSlot)
    }

    /// Waits for the slot (the fallback, which runs on the local model instead of waiting for the limit).
    pub async fn take(&self) -> LocalSlot {
        LocalSlot(
            self.slot
                .clone()
                .acquire_owned()
                .await
                .expect("the local slot is never closed"),
        )
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
        self.up.swap(up, Ordering::SeqCst) != up
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
    loop {
        let changed = local.check(&http).await;
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
