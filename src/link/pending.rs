//! Links started while the agent runs, for the tray to show and start (tray/04-the-tray-agent.md section 4.6, "Link
//! to ▸" and "Waiting for you"): one device flow at a time per cloud, several clouds at once, and the shared
//! backlog's approval. `subscribe` wakes a watcher on every change, so the tray refreshes at once.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::{BACKLOG_TARGET, CLOUD_TARGET, DeviceCode, Link, LinkTarget, TokenStore, machine_info};
use crate::env::Env;

/// What a link waits on: the code a person types, and the page that approves it.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub user_code: String,
    pub page_url: String,
}

/// The shared backlog's approval: the line the menu shows and the page that approves it.
#[derive(Debug, Clone, PartialEq)]
pub struct BacklogWaiting {
    pub label: String,
    pub page_url: String,
}

/// The label of a backlog link that waits on an approval, as against the "Unlinked on ..." prompt.
const APPROVE_LABEL: &str = "Approve the shared backlog";

impl BacklogWaiting {
    pub fn approve(page_url: &str) -> Self {
        Self {
            label: APPROVE_LABEL.into(),
            page_url: page_url.into(),
        }
    }

    pub fn unlinked(host: &str, page_url: &str) -> Self {
        Self {
            label: format!("Unlinked on {host}: link again?"),
            page_url: page_url.into(),
        }
    }
}

/// How a link that was started ended, for the settings page to say (never a token: the flow's own error words).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Linked,
    Failed(String),
}

/// What a start does besides the flow itself.
#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    /// Open the approval page in this computer's browser, as the tray does. The settings page shows it as a link
    /// instead, so a click there opens one tab, not two.
    pub open_browser: bool,
    /// On success, start the link to this shared backlog store next, as `retaskable link` does after
    /// retaskable.com's (nothing when this computer already holds that link).
    pub then_backlog: Option<String>,
}

/// An approval address that is safe to show as a link: only an http(s) one, as the store sent it.
pub fn web_address(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

/// Opens a page in the browser; `super::open_url` in the agent, a recorder in tests.
pub type Opener = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;

#[derive(Default)]
struct State {
    /// A cloud with a flow under way: `None` until its code arrives.
    clouds: BTreeMap<String, Option<Waiting>>,
    backlog: Option<BacklogWaiting>,
    /// How each link started since this run began last ended, until it is started again.
    done: BTreeMap<String, Outcome>,
}

struct Inner {
    store: Arc<dyn TokenStore>,
    http: reqwest::Client,
    machine: serde_json::Value,
    opener: Opener,
    second: Duration,
    state: Mutex<State>,
    changes: watch::Sender<u64>,
}

#[derive(Clone)]
pub struct Links(Arc<Inner>);

impl Links {
    pub fn new(store: Arc<dyn TokenStore>, http: reqwest::Client, env: &Env, version: &str) -> Self {
        Self::with(
            store,
            http,
            machine_info(env, version),
            Arc::new(super::open_url),
            Duration::from_secs(1),
        )
    }

    pub fn with(
        store: Arc<dyn TokenStore>,
        http: reqwest::Client,
        machine: serde_json::Value,
        opener: Opener,
        second: Duration,
    ) -> Self {
        Self(Arc::new(Inner {
            store,
            http,
            machine,
            opener,
            second,
            state: Mutex::default(),
            changes: watch::channel(0).0,
        }))
    }

    /// A receiver that changes whenever any waiting state does.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.changes.subscribe()
    }

    /// The code `url` waits on, while its link does.
    pub fn waiting(&self, url: &str) -> Option<Waiting> {
        self.0.state.lock().unwrap().clouds.get(url).cloned().flatten()
    }

    /// Whether no link to `url` is under way, so `start` would begin one.
    pub fn start_allowed(&self, url: &str) -> bool {
        !self.0.state.lock().unwrap().clouds.contains_key(url)
    }

    pub fn backlog(&self) -> Option<BacklogWaiting> {
        self.0.state.lock().unwrap().backlog.clone()
    }

    pub fn set_backlog(&self, waiting: Option<BacklogWaiting>) {
        self.0.state.lock().unwrap().backlog = waiting;
        self.notify();
    }

    fn notify(&self) {
        self.0.changes.send_modify(|n| *n += 1);
    }

    /// How the last link to `url` ended, while it hasn't been started again.
    pub fn outcome(&self, url: &str) -> Option<Outcome> {
        self.0.state.lock().unwrap().done.get(url).cloned()
    }

    /// Starts the device flow against `url` in a task and opens its `/link?code=…` page. Does nothing, and
    /// returns false, while a link to `url` is already under way. A finished link stores its token as
    /// `retaskable link` does; success, expiry and an error all clear the waiting state.
    pub fn start(&self, url: &str) -> bool {
        self.start_flow(
            url,
            CLOUD_TARGET,
            StartOpts {
                open_browser: true,
                then_backlog: None,
            },
        )
    }

    /// [`start`](Self::start) for retaskable.com or, with `BACKLOG_TARGET`, the shared backlog store (which the tray
    /// also shows under "Waiting for you"). With `then_backlog`, a link that succeeds goes on to the store's.
    pub fn start_flow(&self, url: &str, target: LinkTarget, opts: StartOpts) -> bool {
        let backlog = target.device_path == BACKLOG_TARGET.device_path;
        {
            let mut state = self.0.state.lock().unwrap();
            // The store's own start at launch holds only the tray's line, not a flow under `clouds`.
            let approving = state.backlog.as_ref().is_some_and(|b| b.label == APPROVE_LABEL);
            if state.clouds.contains_key(url) || (backlog && approving) {
                return false;
            }
            state.clouds.insert(url.to_string(), None);
            state.done.remove(url);
        }
        self.notify();
        let (links, url) = (self.clone(), url.to_string());
        tokio::spawn(async move {
            let flow = Link {
                http: links.0.http.clone(),
                store: links.0.store.as_ref(),
                machine: links.0.machine.clone(),
                target,
                second: links.0.second,
            };
            let shown = |code: &DeviceCode| {
                let waiting = Waiting {
                    user_code: code.user_code.clone(),
                    page_url: code.verification_uri_complete.clone(),
                };
                links.0.state.lock().unwrap().clouds.insert(url.clone(), Some(waiting));
                if backlog {
                    links.0.state.lock().unwrap().backlog =
                        Some(BacklogWaiting::approve(&code.verification_uri_complete));
                }
                links.notify();
                if !opts.open_browser {
                    return;
                }
                if let Err(e) = (links.0.opener)(&code.verification_uri_complete) {
                    tracing::warn!("couldn't open the link page for {url}: {e:#}");
                }
            };
            let result = flow.run(&url, shown).await;
            let outcome = match &result {
                Ok(id) => {
                    tracing::info!("Linked to {url} as agent {id}.");
                    Outcome::Linked
                }
                Err(e) => {
                    tracing::warn!("Couldn't link to {url}: {e:#}");
                    Outcome::Failed(format!("{e:#}"))
                }
            };
            {
                let mut state = links.0.state.lock().unwrap();
                state.clouds.remove(&url);
                state.done.insert(url.clone(), outcome);
                if backlog {
                    state.backlog = None;
                }
            }
            links.notify();
            if let (true, Some(next)) = (result.is_ok(), opts.then_backlog.as_deref()) {
                crate::backlog::clear_unlinked_on_site();
                if !matches!(links.0.store.get(next), Ok(Some(_))) {
                    let then = StartOpts {
                        then_backlog: None,
                        ..opts
                    };
                    links.start_flow(next, BACKLOG_TARGET, then);
                }
            }
        });
        true
    }
}
