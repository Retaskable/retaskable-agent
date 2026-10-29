//! The cloud connector (Retaskable.com agent/src/connector.ts, cloud.md section 5): once linked, the agent keeps a
//! WebSocket open to an AgentHub on each cloud for each of its projects (projects.md section 9.1), on the machine's
//! one access token for that cloud. Each says hello, sends a snapshot, and says it's alive in a quiet minute; it
//! reconnects with backoff. Outbound only: nothing listens on a public port.
//!
//! What it reports is what this agent has so far: the machine, its tags, each project's repo and backlog, its
//! galleries' real status and last shoot, and an orchestrator that runs no work yet (`AgentView`). A command from
//! the site (`commands.rs`) is checked and run if it's one this agent has ported — so far Restart, Pause, Resume
//! and Shoot — and refused otherwise.
#[cfg(test)]
mod tests;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};

use crate::commands::{self, Seen};
use crate::galleries;
use crate::link::{CLOUD_TARGET, DeviceTokens, TokenAnswer, TokenStore, cause, environment_name, truncate};
use crate::view::{AgentView, Exit};

/// The name a socket that signs in with a token offers beside it (shared/cloud.ts `SUBPROTOCOL`).
pub const SUBPROTOCOL: &str = "retaskable";

/// WebSocket close codes the hub uses (shared/cloud.ts `CLOSE`).
pub mod close {
    pub const REPLACED: u16 = 4000;
    pub const TOKEN_EXPIRED: u16 = 4001;
    pub const UNLINKED: u16 = 4003;
}

/// How long the connector waits for things (connector.ts's constants); shorter in tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// How often an unlinked agent looks for a refresh token (`retaskable link` in the terminal), and how long a
    /// connected one stays quiet before it says it's alive.
    pub idle: Duration,
    /// How long a connect may take, its token included, before it counts as failed and is tried again.
    pub connect: Duration,
    /// The first reconnect's wait: they back off from this to 60 times it.
    pub backoff: Duration,
    /// A connection that lasted this long was a good one: the next reconnect starts from the first wait again.
    pub stable: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(60),
            connect: Duration::from_secs(30),
            backoff: Duration::from_secs(1),
            stable: Duration::from_secs(30),
        }
    }
}

/// 1 s, 2 s, 4 s … up to 60 s, each with up to half taken off at random (`random` in 0..1) so agents don't reconnect
/// in step.
pub fn backoff(attempt: u32, unit: Duration, random: f64) -> Duration {
    let wait = unit.saturating_mul(2u32.saturating_pow(attempt)).min(unit * 60);
    wait.mul_f64(1.0 - random.clamp(0.0, 1.0) / 2.0)
}

/// A number in 0..1 that differs from one call and one process to the next (each `RandomState` has fresh keys).
fn random() -> f64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u8(0);
    (hasher.finish() >> 11) as f64 / (1u64 << 53) as f64
}

/// One cloud's link: one access token shared by every project's connection, and those connections.
pub struct Connector {
    tokens: Arc<DeviceTokens>,
    tasks: Vec<JoinHandle<()>>,
}

/// What every connection to one cloud shares.
struct Cloud {
    /// For the log: "cloud (staging)".
    label: String,
    tokens: Arc<DeviceTokens>,
    view: Arc<AgentView>,
    timing: Timing,
    exit: Arc<Exit>,
    /// Where `.retaskable/remote.log` goes (cloud.md section 6.3).
    dir: PathBuf,
    /// For the audit log's `cloud` field: the same url every connection's token answer carries.
    url: String,
}

impl Connector {
    /// Starts a connection for each of `view`'s projects to the cloud at `url`. Projects after the first wait until
    /// the first has connected once, so the first is the one that keeps the agent's own hub (projects.md section 9.2).
    /// `exit` is Restart's (the same one the settings page's restart button uses). `dir` is the agent's own data
    /// directory, where every command answered gets logged (`commands::audit`).
    pub fn start(
        url: &str,
        store: Arc<dyn TokenStore>,
        http: reqwest::Client,
        view: Arc<AgentView>,
        timing: Timing,
        exit: Arc<Exit>,
        dir: &Path,
    ) -> Self {
        let tokens = Arc::new(DeviceTokens::new(url.to_string(), CLOUD_TARGET, store, http));
        let label = format!("cloud ({})", environment_name(url).to_lowercase());
        let cloud = Arc::new(Cloud {
            label,
            tokens: tokens.clone(),
            view,
            timing,
            exit,
            dir: dir.to_path_buf(),
            url: url.to_string(),
        });
        let (opened, first_opened) = watch::channel(false);
        let mut opened = Some(opened);
        let tasks = cloud
            .view
            .keys()
            .into_iter()
            .enumerate()
            .map(|(i, project)| {
                let cloud = cloud.clone();
                let opened = if i == 0 { opened.take() } else { None };
                let mut first_opened = first_opened.clone();
                tokio::spawn(async move {
                    if i > 0 {
                        let _ = first_opened.wait_for(|o| *o).await;
                    }
                    run(cloud, project, opened).await;
                })
            })
            .collect();
        Self { tokens, tasks }
    }

    /// Stops every connection, once a token refresh under way has landed (up to `wait`): stopping in the middle of
    /// one loses the token the cloud just rotated to, and unlinks the agent at its next start.
    pub async fn stop(self, wait: Duration) {
        let tasks = self.tasks;
        self.tokens
            .settled(wait, || {
                for task in &tasks {
                    task.abort();
                }
            })
            .await;
    }
}

/// `wss://…/api/agents/connect?project=<id>`, no `project` for the agent's own hub.
fn connect_url(url: &str, project: &str) -> String {
    let base = format!("{}/api/agents/connect", url.replacen("http", "ws", 1));
    if project.is_empty() {
        return base;
    }
    match Url::parse_with_params(&base, [("project", project)]) {
        Ok(u) => u.to_string(),
        Err(_) => base,
    }
}

/// One project's connection: connects, runs it until it closes, and connects again.
async fn run(cloud: Arc<Cloud>, project: String, mut opened: Option<watch::Sender<bool>>) {
    let t = cloud.timing;
    let log = |msg: String| {
        let who = if project.is_empty() {
            String::new()
        } else {
            format!("{project}: ")
        };
        tracing::info!("{}: {who}{msg}", cloud.label);
    };
    let mut attempt = 0u32;
    let next_backoff = |attempt: &mut u32| {
        let wait = backoff(*attempt, t.backoff, random());
        *attempt = attempt.saturating_add(1);
        wait
    };
    let mut stale: Option<String> = None;
    // One project's commands, so a replay is caught across reconnects (commands.ts's `Commands` is one per
    // project's connection too).
    let mut seen = Seen::default();
    loop {
        // A token that never comes (a refresh hanging on the network) is a failed try, not a link that stops.
        let answer = tokio::time::timeout(t.connect, cloud.tokens.access_token(stale.as_deref()))
            .await
            .unwrap_or(TokenAnswer::Unreachable);
        let wait = match answer {
            TokenAnswer::Idle => t.idle,
            TokenAnswer::Unreachable => next_backoff(&mut attempt),
            TokenAnswer::Token { url, access_token } => {
                let ws_url = connect_url(&url, &project);
                // A socket that neither opens nor fails would hold this project off the cloud for good: give it up.
                match tokio::time::timeout(t.connect, open(&ws_url, &access_token)).await {
                    Ok(Ok(ws)) => {
                        log(format!("connected to {url}"));
                        if let Some(opened) = opened.take() {
                            let _ = opened.send(true); // the other projects may connect now
                        }
                        let since = Instant::now();
                        let (code, reason) = connected(ws, &cloud, &project, &log, &mut seen).await;
                        if since.elapsed() >= t.stable {
                            attempt = 0;
                        }
                        if code == close::TOKEN_EXPIRED {
                            // The token ran out: the next connect takes a fresh one, at once.
                            stale = Some(access_token);
                            Duration::ZERO
                        } else {
                            stale = None;
                            let wait = next_backoff(&mut attempt);
                            log(format!(
                                "disconnected: {}; retrying in {} s",
                                why(code, &reason),
                                wait.as_secs()
                            ));
                            wait
                        }
                    }
                    failed => {
                        let why = match failed {
                            Ok(Err(e)) => e,
                            _ => format!("no answer from {url} in {} s", t.connect.as_secs()),
                        };
                        // A connect that never opened may have been refused its token: the next takes a fresh one.
                        stale = Some(access_token);
                        let wait = next_backoff(&mut attempt);
                        log(format!("couldn't connect: {why}; retrying in {} s", wait.as_secs()));
                        wait
                    }
                }
            }
        };
        tokio::time::sleep(wait).await;
    }
}

fn why(code: u16, reason: &str) -> String {
    match code {
        close::UNLINKED => "the agent was unlinked".to_string(),
        close::REPLACED => "another connection replaced this one".to_string(),
        _ if reason.is_empty() => format!("closed ({code})"),
        _ => format!("closed ({code}: {reason})"),
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the socket, signing in with the access token beside the subprotocol.
async fn open(ws_url: &str, access_token: &str) -> Result<Socket, String> {
    let mut request = ws_url.into_client_request().map_err(|e| cause(&e))?;
    let protocols = HeaderValue::from_str(&format!("{SUBPROTOCOL}, {access_token}")).map_err(|e| cause(&e))?;
    request.headers_mut().insert("Sec-WebSocket-Protocol", protocols);
    let (ws, _) = tokio_tungstenite::connect_async(request).await.map_err(|e| cause(&e))?;
    Ok(ws)
}

/// Runs an open connection: hello and a snapshot, then answers what the hub sends, and says it's alive in a quiet
/// minute (sync-health.md section 3.1). The close code and reason when it closes.
async fn connected(ws: Socket, cloud: &Cloud, project: &str, log: &impl Fn(String), seen: &mut Seen) -> (u16, String) {
    let view = &cloud.view;
    let (mut tx, mut rx) = ws.split();
    for msg in [view.hello(project), view.snapshot(project, &cloud.dir)] {
        if let Err(e) = tx.send(Message::text(msg.to_string())).await {
            return (1006, cause(&e));
        }
    }
    let t = cloud.timing;
    let mut alive = tokio::time::interval_at(tokio::time::Instant::now() + t.idle, t.idle);
    // The snapshot just sent has the sizes, pause and galleries as they are now; a later change sends it again.
    let mut sizes = view.lane_sizes.subscribe();
    sizes.mark_unchanged();
    let mut paused = view.paused.subscribe();
    paused.mark_unchanged();
    let mut gallery_status = galleries::subscribe_status_changes();
    gallery_status.mark_unchanged();
    let mut gallery_changes = gallery_status.borrow().get(project).copied().unwrap_or(0);
    loop {
        let reply = tokio::select! {
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(text))) => receive(&text, cloud, project, seen, log),
                Some(Ok(Message::Close(frame))) => {
                    return frame.map_or((1005, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
                }
                Some(Ok(_)) => None,
                Some(Err(e)) => return (1006, cause(&e)),
                None => return (1006, String::new()),
            },
            _ = alive.tick() => Some(json!({ "type": "alive" })),
            Ok(()) = sizes.changed() => Some(view.snapshot(project, &cloud.dir)),
            Ok(()) = paused.changed() => Some(view.snapshot(project, &cloud.dir)),
            Ok(()) = gallery_status.changed() => {
                let count = gallery_status.borrow().get(project).copied().unwrap_or(0);
                if count != gallery_changes {
                    gallery_changes = count;
                    Some(view.snapshot(project, &cloud.dir))
                } else {
                    None
                }
            },
        };
        if let Some(reply) = reply
            && let Err(e) = tx.send(Message::text(reply.to_string())).await
        {
            return (1006, cause(&e));
        }
    }
}

/// The answer to a message from the hub, if it needs one. A command is checked, run (commands.rs) and audited;
/// `watch` finds no session to send; everything else is nothing to act on yet.
fn receive(text: &str, cloud: &Cloud, project: &str, seen: &mut Seen, log: &impl Fn(String)) -> Option<Value> {
    let msg: Value = serde_json::from_str(text).ok()?;
    if msg.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    let id = msg
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 200)?;
    let field = |k: &str| {
        msg.get(k)
            .and_then(Value::as_str)
            .map(|s| truncate(s, 100))
            .unwrap_or_else(|| "?".into())
    };
    let cmd = commands::Command {
        id: id.to_string(),
        name: field("name"),
        args: msg.get("args").cloned().unwrap_or(Value::Null),
        issued_by: field("issuedBy"),
        issued_at: msg.get("issuedAt").and_then(Value::as_i64).unwrap_or(0),
        expires_at: msg.get("expiresAt").and_then(Value::as_i64).unwrap_or(0),
    };
    let answer = commands::run(&cmd, project, seen, &cloud.view, cloud.exit.as_ref(), &cloud.dir);
    let outcome = if answer.get("type").and_then(Value::as_str) == Some("ack") {
        "done".to_string()
    } else {
        answer.get("reason").and_then(Value::as_str).unwrap_or("?").to_string()
    };
    log(format!("{} from {}: {outcome}", cmd.name, cmd.issued_by));
    commands::audit(&cloud.dir, &cmd, &answer, &cloud.url);
    Some(answer)
}
