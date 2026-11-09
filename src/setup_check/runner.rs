//! first-run-check.md section 3.1, "The runner": keeps the latest setup checks, runs them one at a
//! time, and says when a finished run's result differs from the one before. Ported from
//! `SetupCheckRunner` in `agent/src/setupCheck.ts`.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::watch;

use super::{SetupCheck, SetupChecks};

/// How often the checks run again, whether or not a site is linked.
pub const RERUN_EVERY: Duration = Duration::from_secs(600);

/// One run of the checks.
pub type Probe = Box<dyn Fn() -> BoxFuture<'static, SetupChecks> + Send + Sync>;
/// Waits the given time (the clock tests replace).
pub type Sleep = Box<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct SetupCheckRunner {
    latest: Arc<Mutex<Option<SetupChecks>>>,
    /// Held for the whole of a run, so runs never overlap.
    running: tokio::sync::Mutex<()>,
    /// Runs finished so far, so a caller that waited out another's run can tell.
    finished: AtomicU64,
    changed: watch::Sender<Option<SetupChecks>>,
    probe: Probe,
    sleep: Sleep,
}

impl SetupCheckRunner {
    pub fn new(probe: Probe, sleep: Sleep) -> Self {
        Self {
            latest: Arc::default(),
            running: tokio::sync::Mutex::new(()),
            finished: AtomicU64::new(0),
            changed: watch::channel(None).0,
            probe,
            sleep,
        }
    }

    /// With the real clock.
    pub fn with_probe(probe: Probe) -> Self {
        Self::new(probe, Box::new(|d| Box::pin(tokio::time::sleep(d))))
    }

    /// The latest result; `None` until the first run finishes.
    pub fn latest(&self) -> Option<SetupChecks> {
        self.latest.lock().unwrap().clone()
    }

    /// The cell the runner writes, for readers that hold the plain latest result.
    pub fn shared(&self) -> Arc<Mutex<Option<SetupChecks>>> {
        self.latest.clone()
    }

    /// A receiver that ticks when a finished run's result differs from the one before (the first
    /// result counts); its value is that result.
    pub fn subscribe(&self) -> watch::Receiver<Option<SetupChecks>> {
        self.changed.subscribe()
    }

    /// Runs the checks now. A call made while a run is going waits for it and returns its result.
    pub async fn run_now(&self) -> SetupChecks {
        let seen = self.finished.load(Ordering::SeqCst);
        let _turn = self.running.lock().await;
        if self.finished.load(Ordering::SeqCst) != seen
            && let Some(done) = self.latest()
        {
            return done;
        }
        let checks = (self.probe)().await;
        *self.latest.lock().unwrap() = Some(checks.clone());
        self.finished.fetch_add(1, Ordering::SeqCst);
        self.changed.send_if_modified(|prev| {
            let differs = prev.as_ref() != Some(&checks);
            if differs {
                *prev = Some(checks.clone());
            }
            differs
        });
        checks
    }

    /// Runs at start, hands that result to `first` (the report), then runs again every ten minutes.
    pub fn spawn<F>(self: &Arc<Self>, first: F)
    where
        F: FnOnce(SetupChecks) -> BoxFuture<'static, ()> + Send + 'static,
    {
        let runner = self.clone();
        tokio::spawn(async move {
            let checks = runner.run_now().await;
            first(checks).await;
            loop {
                (runner.sleep)(RERUN_EVERY).await;
                runner.run_now().await;
            }
        });
    }
}

impl std::fmt::Debug for SetupCheckRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupCheckRunner")
            .field("latest", &self.latest())
            .finish()
    }
}

/// Two views are equal when they share a runner.
impl PartialEq for SetupCheckRunner {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

/// A runner whose probe finds nothing out (every check unknown), for views built without one.
impl Default for SetupCheckRunner {
    fn default() -> Self {
        let unknown = || SetupCheck::unknown("not checked", None);
        Self::with_probe(Box::new(move || {
            Box::pin(async move {
                SetupChecks {
                    git: unknown(),
                    gh_installed: unknown(),
                    gh: unknown(),
                    claude: unknown(),
                    claude_signed_in: unknown(),
                    cursor: unknown(),
                    pi: None,
                    codex: None,
                    docker: None,
                }
            })
        }))
    }
}
