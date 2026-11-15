//! Links started while the agent runs, for the tray to show and start (tray/04-the-tray-agent.md section 4.6, "Link
//! to ▸" and "Waiting for you"): one device flow at a time per cloud, several clouds at once, and the shared
//! backlog's approval. `subscribe` wakes a watcher on every change, so the tray refreshes at once.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::{CLOUD_TARGET, DeviceCode, Link, TokenStore, machine_info};
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

impl BacklogWaiting {
    pub fn approve(page_url: &str) -> Self {
        Self {
            label: "Approve the shared backlog".into(),
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

/// Opens a page in the browser; `super::open_url` in the agent, a recorder in tests.
pub type Opener = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;

#[derive(Default)]
struct State {
    /// A cloud with a flow under way: `None` until its code arrives.
    clouds: BTreeMap<String, Option<Waiting>>,
    backlog: Option<BacklogWaiting>,
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

    /// Starts the device flow against `url` in a task and opens its `/link?code=…` page. Does nothing, and
    /// returns false, while a link to `url` is already under way. A finished link stores its token as
    /// `retaskable link` does; success, expiry and an error all clear the waiting state.
    pub fn start(&self, url: &str) -> bool {
        match self.0.state.lock().unwrap().clouds.entry(url.to_string()) {
            std::collections::btree_map::Entry::Occupied(_) => return false,
            std::collections::btree_map::Entry::Vacant(v) => v.insert(None),
        };
        self.notify();
        let (links, url) = (self.clone(), url.to_string());
        tokio::spawn(async move {
            let flow = Link {
                http: links.0.http.clone(),
                store: links.0.store.as_ref(),
                machine: links.0.machine.clone(),
                target: CLOUD_TARGET,
                second: links.0.second,
            };
            let shown = |code: &DeviceCode| {
                let waiting = Waiting {
                    user_code: code.user_code.clone(),
                    page_url: code.verification_uri_complete.clone(),
                };
                links.0.state.lock().unwrap().clouds.insert(url.clone(), Some(waiting));
                links.notify();
                if let Err(e) = (links.0.opener)(&code.verification_uri_complete) {
                    tracing::warn!("couldn't open the link page for {url}: {e:#}");
                }
            };
            match flow.run(&url, shown).await {
                Ok(id) => tracing::info!("Linked to {url} as agent {id}."),
                Err(e) => tracing::warn!("Couldn't link to {url}: {e:#}"),
            }
            links.0.state.lock().unwrap().clouds.remove(&url);
            links.notify();
        });
        true
    }
}
