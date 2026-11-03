//! The link side of reporting Retaskable's own bugs (bug-reports.md sections 3.1-3.2, `agent/src/connector.ts`'s
//! `bugReportsOn`, `knownBugs` and `bugReport`): what the project's hub said in `bug_reports { on, known }`, and
//! sending `bug_report` and waiting for the `bug_reported` with the same `ref`. Kept per connection: the hub says
//! again after each hello, and everything here is off again once the connection closes.
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::release::Build;
use crate::triage::KnownBug;
use crate::triage::bug_report::BugReportMessage;

/// The hub's `bug_reported { ref, ok, duplicate?, reason?, itemId? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReported {
    pub ok: bool,
    pub duplicate: bool,
    pub reason: Option<String>,
    pub item_id: Option<String>,
}

/// What sending a bug came to: the hub's answer, or why nothing was answered. Never an error: the caller's own
/// work goes on either way, and the reason is logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BugOutcome {
    Answered(BugReported),
    NotSent(String),
}

#[derive(Default)]
struct State {
    on: bool,
    known: Vec<KnownBug>,
    /// The open connection's outbound messages; none while it isn't connected.
    out: Option<mpsc::UnboundedSender<Value>>,
    waiters: HashMap<String, oneshot::Sender<BugReported>>,
    sent: u64,
}

/// One project's hub connection's bug-report consent, known bugs and sender. Shared between the connection (which
/// fills it in) and the rest of the agent (which reads it and sends).
pub struct BugLink {
    state: Mutex<State>,
    /// How long an answer is waited for (the connect timeout).
    timeout: Duration,
}

impl BugLink {
    pub fn new(timeout: Duration) -> Self {
        Self {
            state: Mutex::default(),
            timeout,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the hub said its person agreed: false until it does, and while it isn't connected.
    pub fn consent(&self) -> bool {
        self.state().on
    }

    /// The bugs already on Retaskable's board the hub listed, with the ones sent since; none while off.
    pub fn known(&self) -> Vec<KnownBug> {
        self.state().known.clone()
    }

    /// A connection opened: messages put on `out` go up it.
    pub(super) fn opened(&self, out: mpsc::UnboundedSender<Value>) {
        self.state().out = Some(out);
    }

    /// The connection closed: off again until the next hello's `bug_reports`, and anything waiting gives up.
    pub(super) fn closed(&self) {
        let mut state = self.state();
        *state = State {
            sent: state.sent,
            ..State::default()
        };
    }

    /// The hub's `bug_reports { on, known }`.
    pub(super) fn consent_said(&self, msg: &Value) {
        let on = msg.get("on").and_then(Value::as_bool).unwrap_or(false);
        let known = msg
            .get("known")
            .and_then(Value::as_array)
            .filter(|_| on)
            .map(|list| {
                list.iter()
                    .filter_map(|b| {
                        let id = b.get("id")?.as_str().filter(|id| !id.is_empty())?;
                        let title = b.get("title").and_then(Value::as_str).unwrap_or_default();
                        Some(KnownBug {
                            id: id.to_string(),
                            title: title.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut state = self.state();
        state.on = on;
        state.known = known;
    }

    /// The hub's `bug_reported`, to whoever sent the bug with its `ref`.
    pub(super) fn answered(&self, msg: &Value) {
        let Some(r) = msg.get("ref").and_then(Value::as_str) else {
            return;
        };
        let text = |k: &str| msg.get(k).and_then(Value::as_str).map(str::to_string);
        let answer = BugReported {
            ok: msg.get("ok").and_then(Value::as_bool).unwrap_or(false),
            duplicate: msg.get("duplicate").and_then(Value::as_bool).unwrap_or(false),
            reason: text("reason"),
            item_id: text("itemId"),
        };
        if let Some(waiter) = self.state().waiters.remove(r) {
            let _ = waiter.send(answer);
        }
    }

    /// Sends `bug` as `bug_report` and waits for the hub's `bug_reported`. `sameAs` goes only when it's an id the
    /// hub listed. Unconnected, off, or no answer in time is a [`BugOutcome::NotSent`] with why, and is logged.
    pub async fn send(&self, bug: &BugReportMessage, version: &str) -> BugOutcome {
        let outcome = self.try_send(bug, version).await;
        match &outcome {
            BugOutcome::NotSent(why) => tracing::info!("bug report not sent: {why}"),
            BugOutcome::Answered(a) if !a.ok => tracing::info!(
                "bug report refused: {}",
                a.reason.as_deref().unwrap_or("no reason given")
            ),
            BugOutcome::Answered(_) => {}
        }
        outcome
    }

    async fn try_send(&self, bug: &BugReportMessage, version: &str) -> BugOutcome {
        let (r, answer) = {
            let mut state = self.state();
            let Some(out) = state.out.clone() else {
                return BugOutcome::NotSent("the project isn't connected to retaskable.com".into());
            };
            if !state.on {
                return BugOutcome::NotSent("retaskable.com hasn't said its person agreed to bug reports".into());
            }
            state.sent += 1;
            let r = format!("bug-{}", state.sent);
            let same_as = bug
                .same_as
                .as_deref()
                .filter(|id| state.known.iter().any(|k| k.id == *id));
            let mut msg = json!({
                "type": "bug_report",
                "ref": r,
                "signature": bug.signature,
                "title": bug.title,
                "details": bug.details,
                "version": version.chars().take(100).collect::<String>(),
            });
            if let Some(id) = same_as {
                msg["sameAs"] = json!(id);
            }
            let (tx, rx) = oneshot::channel();
            state.waiters.insert(r.clone(), tx);
            if out.send(msg).is_err() {
                state.waiters.remove(&r);
                return BugOutcome::NotSent("the connection closed".into());
            }
            (r, rx)
        };
        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(answered)) => {
                if answered.ok
                    && let Some(id) = &answered.item_id
                {
                    let mut state = self.state();
                    if !state.known.iter().any(|k| k.id == *id) {
                        state.known.insert(
                            0,
                            KnownBug {
                                id: id.clone(),
                                title: bug.title.clone(),
                            },
                        );
                    }
                }
                BugOutcome::Answered(answered)
            }
            Ok(Err(_)) => BugOutcome::NotSent("the connection closed before it was answered".into()),
            Err(_) => {
                self.state().waiters.remove(&r);
                BugOutcome::NotSent(format!("no answer in {} s", self.timeout.as_secs()))
            }
        }
    }
}

/// What the orchestrator needs of a project's bug-report link: the hub's consent and known bugs, and sending. A trait so the
/// driver's tests can stand in a fake link.
pub trait BugHub: Send + Sync {
    fn consent(&self) -> bool;
    fn known(&self) -> Vec<KnownBug>;
    fn send<'a>(
        &'a self,
        bug: &'a BugReportMessage,
        version: &'a str,
    ) -> Pin<Box<dyn Future<Output = BugOutcome> + Send + 'a>>;
}

impl BugHub for BugLink {
    fn consent(&self) -> bool {
        BugLink::consent(self)
    }
    fn known(&self) -> Vec<KnownBug> {
        BugLink::known(self)
    }
    fn send<'a>(
        &'a self,
        bug: &'a BugReportMessage,
        version: &'a str,
    ) -> Pin<Box<dyn Future<Output = BugOutcome> + Send + 'a>> {
        Box::pin(BugLink::send(self, bug, version))
    }
}

/// What a bug report says the build is: a release build's version and commit, else the checkout's version and
/// short commit (`bugReport.ts`'s `version`).
pub fn bug_version(build: Option<&Build>, version: &str, checkout: &Path) -> String {
    if let Some(b) = build {
        return format!("{} ({})", b.version, b.commit);
    }
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(checkout)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if sha.is_empty() {
        version.to_string()
    } else {
        format!("{version} ({sha})")
    }
}
