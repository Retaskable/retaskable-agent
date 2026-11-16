//! A project's socket to its backlog store object (Retaskable.com's `agent/src/backlogSocket.ts`, backlog.md section
//! 6.3): `GET /v1/repos/<repo id>/socket` with the store's access token in the `Authorization` header. While it is open
//! the agent's leases don't run out (the store gives one with no expiry while its holder's socket is open, and 15
//! minutes after the last closes), and the store pushes what changed down it: `changed`, `drafts_changed`,
//! `settings_changed`, `paused_changed`, `tryouts_changed`, `tryout_problem`, `draft_answer`, `facts_changed`,
//! `ideas_changed` and `work_available`; each opening is `reconnected`. It reconnects with the cloud connector's
//! backoff and says `ping` now and then, which the store answers `pong` without waking the object. Outbound only,
//! like the cloud link, and it only listens: operations stay on HTTP (`backlog::Client`), and a push only wakes a
//! loop that then asks the store (`wake`), so a push is never a command and one missed (the socket down) is only a
//! later read: the polls go on as they did, and nothing here weakens failing closed.
//!
//! Not ported: a project that isn't on GitHub (its own backlog, reached with a project grant, code-storage.md section
//! 3.5, whose socket is renewed with `{ type: "grant" }` and ends with 4408), since this agent names a backlog only by
//! its repo's id; and `reconnect()`, which the TypeScript agent calls after linking from its UI: here an agent that
//! isn't linked looks again once a minute, which takes the link up the same way.
pub mod live;
#[cfg(test)]
mod tests;
mod transport;
pub mod wake;

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

pub use self::transport::{Conn, Incoming, Outgoing, Transport, Wire};
use crate::connector::{backoff, random};
use crate::link::{DeviceTokens, TokenAnswer};

/// The store's close code for an agent unlinked on its `/agents` page (`CLOSE_UNLINKED` in the store's
/// `projectBacklog.ts`).
pub const CLOSE_UNLINKED: u16 = 4401;

/// What the store pushes (section 6.3).
#[derive(Debug, Clone, PartialEq)]
pub enum Push {
    Changed {
        items: Vec<String>,
    },
    DraftsChanged {
        drafts: Vec<String>,
    },
    WorkAvailable,
    SettingsChanged {
        version: f64,
    },
    /// The project was paused or resumed: read it again.
    PausedChanged,
    /// The try-out queue changed: read it again.
    TryoutsChanged,
    /// A person found a problem with a try-out this agent added.
    TryoutProblem {
        key: String,
        notes: String,
    },
    /// A person answered the questions of a draft this agent holds.
    DraftAnswer {
        id: String,
    },
    /// Results of the project changed (smoke, Lighthouse, the expert queue, deploy requests, ...): read those kinds
    /// again. No kinds means any may have.
    FactsChanged {
        kinds: Vec<String>,
    },
    /// The project's ideas changed.
    IdeasChanged,
    /// The socket opened: anything pushed while it was down was missed, so what waits on pushes looks again.
    Reconnected,
}

/// The strings in `v` when it is an array, else none (`stringsIn`, shared/text.ts).
fn strings_in(v: Option<&Value>) -> Option<Vec<String>> {
    let items = v?.as_array()?;
    Some(items.iter().filter_map(|s| s.as_str().map(String::from)).collect())
}

/// One text frame as a push, or `None` for `pong`, a frame that isn't JSON, a type this agent doesn't know and a push
/// missing what it needs (`receive` in `backlogSocket.ts`: all of them are ignored, none is an error).
pub fn parse(text: &str) -> Option<Push> {
    if text == "pong" {
        return None;
    }
    let msg: Value = serde_json::from_str(text).ok()?;
    let text_of = |k: &str| msg.get(k).and_then(Value::as_str).map(String::from);
    Some(match msg.get("type")?.as_str()? {
        "work_available" => Push::WorkAvailable,
        "changed" => Push::Changed {
            items: strings_in(msg.get("items"))?,
        },
        "drafts_changed" => Push::DraftsChanged {
            drafts: strings_in(msg.get("drafts"))?,
        },
        "settings_changed" => Push::SettingsChanged {
            version: msg.get("version")?.as_f64()?,
        },
        "paused_changed" => Push::PausedChanged,
        "tryouts_changed" => Push::TryoutsChanged,
        "tryout_problem" => Push::TryoutProblem {
            key: text_of("key")?,
            notes: text_of("notes").unwrap_or_default(),
        },
        "draft_answer" => Push::DraftAnswer { id: text_of("id")? },
        "facts_changed" => Push::FactsChanged {
            kinds: strings_in(msg.get("kinds")).unwrap_or_default(),
        },
        "ideas_changed" => Push::IdeasChanged,
        _ => return None,
    })
}

/// How long the socket waits for things (`backlogSocket.ts`'s constants); shorter in tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How often it says `ping`: under Cloudflare's 100 s idle cutoff.
    pub keepalive: Duration,
    /// How often an agent that isn't linked to the store looks again.
    pub idle: Duration,
    /// How long a connect may take, its token included, before it counts as failed. The TypeScript agent has no
    /// limit, so a connect that neither opens nor fails holds its socket off for good; the cloud link's is 30 s.
    pub connect: Duration,
    /// The first reconnect's wait: they back off from this to 60 times it.
    pub backoff: Duration,
    /// A connection that lasted this long was a good one: the next reconnect starts from the first wait again.
    pub stable: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            keepalive: Duration::from_secs(50),
            idle: Duration::from_secs(60),
            connect: Duration::from_secs(30),
            backoff: Duration::from_secs(1),
            stable: Duration::from_secs(30),
        }
    }
}

/// Where the store's access token comes from.
pub trait Tokens: Send + Sync + 'static {
    /// `stale`: a token the store refused, which isn't handed out again.
    fn access_token(&self, stale: Option<String>) -> BoxFuture<'static, TokenAnswer>;
}

impl Tokens for Arc<DeviceTokens> {
    fn access_token(&self, stale: Option<String>) -> BoxFuture<'static, TokenAnswer> {
        let tokens = self.clone();
        Box::pin(async move { DeviceTokens::access_token(&tokens, stale.as_deref()).await })
    }
}

/// What a project's socket is made of.
pub struct Deps {
    /// The project, for the log.
    pub project: String,
    pub tokens: Arc<dyn Tokens>,
    pub transport: Arc<dyn Transport>,
    /// The repo's GitHub id, which names the backlog; `None` when it can't be learned yet.
    pub repo_id: Arc<dyn Fn() -> BoxFuture<'static, Option<u64>> + Send + Sync>,
    pub pushed: Arc<dyn Fn(Push) + Send + Sync>,
    pub timing: Timing,
    /// A number in 0..1, so reconnects don't land in step.
    pub random: fn() -> f64,
}

impl Deps {
    /// The real timing and randomness; the caller names the rest.
    pub fn new(
        project: String,
        tokens: Arc<dyn Tokens>,
        repo_id: Arc<dyn Fn() -> BoxFuture<'static, Option<u64>> + Send + Sync>,
        pushed: Arc<dyn Fn(Push) + Send + Sync>,
    ) -> Self {
        Self {
            project,
            tokens,
            transport: Arc::new(Wire),
            repo_id,
            pushed,
            timing: Timing::default(),
            random,
        }
    }
}

/// A running socket: dropped or stopped, it closes the connection.
pub struct Handle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Handle {
    /// Closes the socket (1000, "agent stopping") and waits for the task to end.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

/// Starts one project's socket.
pub fn start(deps: Deps) -> Handle {
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(run(deps, stopped));
    Handle { stop, task }
}

/// How one open socket ended.
enum End {
    Stopped,
    Closed(u16, String),
}

/// Waits `wait`; whether the socket was stopped meanwhile (a dropped [`Handle`] counts).
async fn stopped_within(stop: &mut watch::Receiver<bool>, wait: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(wait) => false,
        _ = stop.changed() => true,
    }
}

async fn run(deps: Deps, mut stop: watch::Receiver<bool>) {
    let t = deps.timing;
    let log = |msg: String| tracing::info!("backlog: {}: {msg}", deps.project);
    let mut attempt = 0u32;
    let next_backoff = |attempt: &mut u32| {
        let wait = backoff(*attempt, t.backoff, (deps.random)());
        *attempt = attempt.saturating_add(1);
        wait
    };
    let mut stale: Option<String> = None;
    let mut told_why = String::new();
    loop {
        let wait = 'once: {
            // A token that never comes (a refresh hanging on the network) is a failed try, not a link that stops.
            let answer = tokio::time::timeout(t.connect, deps.tokens.access_token(stale.take()))
                .await
                .unwrap_or(TokenAnswer::Unreachable);
            let (store, token) = match answer {
                TokenAnswer::Idle => break 'once t.idle, // not linked: look again in a minute
                TokenAnswer::Unreachable => break 'once next_backoff(&mut attempt),
                TokenAnswer::Token { url, access_token } => (url, access_token),
            };
            let Some(repo) = (deps.repo_id)().await else {
                let why = "gh couldn't say the repo's id".to_string();
                if told_why != why {
                    log(format!("can't connect yet: {why}"));
                    told_why = why;
                }
                break 'once next_backoff(&mut attempt);
            };
            told_why.clear();
            let url = format!("{}/v1/repos/{repo}/socket", store.replacen("http", "ws", 1));
            let opened = tokio::time::timeout(t.connect, deps.transport.open(&url, &token)).await;
            let conn = match opened {
                Ok(Ok(conn)) => conn,
                failed => {
                    let why = match failed {
                        Ok(Err(e)) => e,
                        _ => format!("no answer in {} s", t.connect.as_secs()),
                    };
                    // A socket that never opened may have been refused its token (the upgrade's 401): the next try
                    // takes a fresh one.
                    stale = Some(token);
                    let wait = next_backoff(&mut attempt);
                    log(format!(
                        "couldn't connect to {url}: {why}; retrying in {} s",
                        wait.as_secs()
                    ));
                    break 'once wait;
                }
            };
            log(format!("connected to {store} for repo {repo}"));
            let since = Instant::now();
            let end = session(conn, &deps, &mut stop).await;
            let (code, reason) = match end {
                End::Stopped => return,
                End::Closed(code, reason) => (code, reason),
            };
            if since.elapsed() >= t.stable {
                attempt = 0;
            }
            if code == CLOSE_UNLINKED {
                // The next refresh is refused and forgets the link; until it's linked again, look once a minute.
                log("this agent was unlinked from the backlog store".into());
                stale = Some(token);
                break 'once Duration::ZERO;
            }
            let wait = next_backoff(&mut attempt);
            let why = if reason.is_empty() {
                code.to_string()
            } else {
                format!("{code}: {reason}")
            };
            log(format!("disconnected ({why}); retrying in {} s", wait.as_secs()));
            wait
        };
        if stopped_within(&mut stop, wait).await {
            return;
        }
    }
}

/// An open socket: tells the loop it opened, says `ping` now and then and hands on what the store pushes, until it
/// closes or the socket is stopped.
async fn session(mut conn: Conn, deps: &Deps, stop: &mut watch::Receiver<bool>) -> End {
    let keepalive = deps.timing.keepalive;
    let mut alive = tokio::time::interval_at(Instant::now() + keepalive, keepalive);
    alive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    (deps.pushed)(Push::Reconnected);
    loop {
        tokio::select! {
            _ = stop.changed() => {
                let _ = conn.outgoing.send(Outgoing::Close(1000, "agent stopping".into())).await;
                return End::Stopped;
            }
            _ = alive.tick() => {
                if conn.outgoing.send(Outgoing::Text("ping".into())).await.is_err() {
                    return End::Closed(1006, "couldn't send".into());
                }
            }
            frame = conn.incoming.recv() => match frame {
                Some(Incoming::Text(text)) => {
                    if let Some(push) = parse(&text) {
                        (deps.pushed)(push);
                    }
                }
                Some(Incoming::Closed(code, reason)) => return End::Closed(code, reason),
                None => return End::Closed(1006, String::new()),
            },
        }
    }
}

/// A channel pair a test holds the far ends of.
#[cfg(test)]
pub(crate) fn pair() -> (
    Conn,
    tokio::sync::mpsc::Sender<Incoming>,
    tokio::sync::mpsc::Receiver<Outgoing>,
) {
    use tokio::sync::mpsc;
    let (in_tx, incoming) = mpsc::channel(16);
    let (outgoing, out_rx) = mpsc::channel(16);
    (Conn { incoming, outgoing }, in_tx, out_rx)
}
