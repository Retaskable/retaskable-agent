//! The agent's side of the shared backlog store (docs/specs/agent/02-principles.md: "The store is
//! the source of truth... When the store can't be reached it stops and waits: it fails closed, and
//! never falls back to a local list."): `list`, `claim_next`, `drafts`, the after-merge try-out
//! queue (`tryouts_list`, `tryout_add`, `tryout_result`, `tryout_groups`, `reopen`,
//! docs/specs/agent/06.4-playtests.md) and a named project-wide lock (`chore_lock`), each read over
//! `POST <store>/v1/repos/<repo id>/ops`
//! (Retaskable.com's docs/specs/backlog.md sections 3.1 and 6.2, and backlog/06.6-handing-out-work.md
//! for `claim_next`), and `auto_link`, this machine's own link to the store
//! (docs/specs/installers/04-every-agent-on-the-shared-backlog-shared.md). A repo's numeric GitHub
//! id, and wiring a linked access token in, come from elsewhere (not ported here yet); every way an
//! operation can fail — no address, no network, a refused or unreadable answer — comes back as a
//! `BacklogError` and never as a guessed or empty answer, so a caller can't mistake "couldn't ask"
//! for "nothing to do".
use crate::env::Env;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::link::{self, DeviceCode, TokenStore};

/// Every environment and agent uses the one production store (backlog.md section 3.1): no per-project
/// override, only this default and `RETASKABLE_BACKLOG_URL`.
pub const DEFAULT_URL: &str = "https://backlog.retaskable.com";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The store this agent reads: `RETASKABLE_BACKLOG_URL`, else the default. No trailing slash.
pub fn url(env: &Env) -> String {
    let raw = env
        .get("RETASKABLE_BACKLOG_URL")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    raw.trim_end_matches('/').to_string()
}

/// Every agent on the shared backlog (installers/04-every-agent-on-the-shared-backlog-shared.md): starts the
/// store's device flow by itself, the same way `link::Link` links this machine to a cloud, only when its URL
/// is good and this machine holds no link to it yet. Called once at this agent's start when it already holds
/// a retaskable.com link, and once right after a retaskable.com link is approved — never on a timer, so a
/// failed or expired code waits for the next start rather than looping. `Ok(None)` when there was nothing to
/// start (already linked, or a bad URL); the agent id once approved.
pub async fn auto_link(
    store: Arc<dyn TokenStore>,
    http: reqwest::Client,
    env: &Env,
    version: &str,
    on_code: impl FnOnce(&DeviceCode),
) -> Result<Option<u64>> {
    let backlog_url = url(env);
    if let Err(e) = link::valid_http_url(&backlog_url, "backlog store") {
        tracing::warn!("backlog store URL isn't valid: {e:#}");
        return Ok(None);
    }
    if matches!(store.get(&backlog_url), Ok(Some(_))) {
        return Ok(None); // already linked
    }
    let flow = link::Link {
        http,
        store: store.as_ref(),
        machine: link::machine_info(env, version),
        target: link::BACKLOG_TARGET,
        second: Duration::from_secs(1),
    };
    flow.run(&backlog_url, on_code).await.map(Some)
}

/// backlog/06.1-an-item.md: an item's status in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Held,
    Todo,
    Claimed,
    Building,
    InReview,
    Merging,
    NeedsPerson,
    OnBranch,
    OnMain,
    InProduction,
    Skipped,
}

/// One item as `list` answers it (backlog/06.1-an-item.md, backlog/06.2-operations.md): only the
/// fields this agent needs to say what work there is and in what order. Other fields the store sends
/// (`claimedBy`, `history`, `lease`, ...) are read by later pieces, not dropped by this one — `Item`
/// just doesn't model them yet, and serde leaves unknown fields alone.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub title: String,
    pub status: ItemStatus,
    pub rank: u32,
    #[serde(default)]
    pub do_next: bool,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub spec_refs: Vec<String>,
    #[serde(default)]
    pub slice: u32,
    #[serde(default)]
    pub remaining: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub milestone: Option<String>,
    /// While a try-out's problem reopened it (`reopen`); missing or null once the fix merges.
    #[serde(default)]
    pub reopened: Option<Reopened>,
}

/// `reopened` (backlog/06.1-an-item.md): `{ notes, at, by }` while a try-out's problem reopened the
/// item, until its fix merges. Only whether this is `Some` is read today, so a `notes`/`at` the
/// store ever sent without the other doesn't fail the whole item's (or `claim_next`'s) read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reopened {
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
}

/// A spec draft as `drafts` answers it (backlog/06.2-operations.md): only what release notes match a
/// merged PR by.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub id: String,
    #[serde(default)]
    pub titles: Vec<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
}

/// What `claim_next` sends about this agent (backlog/06.6-handing-out-work.md section 6.6.1): its
/// machine name, the host of the retaskable.com it links to, the project's base branch, and this
/// computer's tags (agent/07.5-what-a-claim-and-a-give-back-send.md), left out when it has none so
/// the store matches them against the project's `settings.needs`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimNextAgent {
    pub machine: String,
    pub environment: String,
    pub base_branch: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// What `claim_next` answers: an item claimed, with the lease token that goes with it, or nothing
/// to claim right now (paused, or nothing ready).
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimNextAnswer {
    Item { item: Box<Item>, token: String },
    Nothing,
}

/// What `tryout_add` sends for a merged slice's after-merge try-out (backlog/06-data-and-operations.md
/// section 6.2, docs/specs/agent/06.4-playtests.md): keyed `<itemId>@<mergedAt>` in the store, so
/// sending the same one again changes nothing. The pre-merge `onBranch` variant (keyed by branch and
/// sha instead of `mergedAt`) isn't sent here yet — nothing in this crate tracks an item's approved
/// branch and sha to send it with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TryoutAdd {
    pub item_id: String,
    pub merged_at: String,
    pub title: String,
    pub what: String,
    pub steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    pub machine: String,
}

/// `onBranch` on a [`Tryout`] still waiting on its PR to merge (docs/specs/agent/06.4-playtests.md's
/// `"on"` mode): present only before the item merges, keyed by branch and sha instead of `mergedAt`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OnBranch {
    pub branch: String,
    pub sha: String,
}

/// One entry in the shared try-out queue, as `tryouts_list` answers it (shared/backlogTypes.ts's
/// `Tryout`), trimmed to what grouping needs: whether it's still on a branch (not grouped until it
/// merges, docs/specs/agent/06.4-playtests.md) and what a playtester would be told. The fields a
/// played-out row carries (`status`, `result`, `addedBy`, `fix`, ...) aren't modelled yet — nothing
/// in this port reads the done queue — and serde leaves them alone.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tryout {
    pub key: String,
    pub item_id: String,
    pub title: String,
    pub what: String,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub on_branch: Option<OnBranch>,
}

/// The grouping last sent for the waiting queue (shared/backlogTypes.ts's `TryoutGrouping`),
/// trimmed to what [`crate::playtest_groups::should_group`] needs to decide whether to run another
/// round: its `groups` themselves aren't read back here, only sent (`tryout_groups`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TryoutGrouping {
    pub signature: String,
    pub at: i64,
}

/// What `tryouts_list` answers (backlog/06-data-and-operations.md section 6.2,
/// docs/specs/agent/06.4-playtests.md): the after-merge try-outs still waiting on a person, and the
/// grouping last sent for them, if any. `done` and `more` (the played-out page) aren't read here
/// yet, matching `agent/src/storeTryouts.ts`'s own `readTryouts`, which calls this with no
/// arguments and reads only these two fields back.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TryoutsList {
    pub waiting: Vec<Tryout>,
    #[serde(default)]
    pub groups: Option<TryoutGrouping>,
}

/// A person's answer to one try-out, sent with `tryout_result` (backlog/06-data-and-operations.md
/// section 6.2): once per key, after which the store triggers its own `tryout_problem` or
/// merge-cleanup follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TryoutOutcome {
    Passed,
    Problem,
}

/// What `tryout_groups` sends for one themed session (backlog/06-data-and-operations.md section 6.2,
/// docs/specs/agent/06.4-playtests.md "Grouped into sessions"): the wire shape of
/// [`crate::playtest_groups::PlaytestGroup`], kept separate so this module doesn't depend on that
/// feature's own types.
#[derive(Debug, Clone, Serialize)]
pub struct TryoutGroup {
    pub title: String,
    pub why: String,
    pub keys: Vec<String>,
}

/// What `reopen` sends about the try-out's problem (docs/specs/agent/06.4-playtests.md "Problem
/// reopens the item itself"; `agent/src/reopen.ts`'s `reopen`): the playtest's own `what`/`steps` and
/// who said it, so a store that keeps the item as it is (or an older one that doesn't know `reopen`)
/// still gives the caller enough to queue a playtest-fix chore with, as before.
#[derive(Debug, Clone)]
pub struct ReopenRequest<'a> {
    pub item_id: &'a str,
    pub notes: &'a str,
    pub by: Option<&'a str>,
    pub what: Option<&'a str>,
    pub steps: Option<&'a [String]>,
}

/// What `reopen` answers: the item handed straight back with a lease token, the way `claim_next`
/// hands one out, so the computer that built it resumes there; or kept as it is (being built, or an
/// older store that doesn't know `reopen`) — [`crate::reopen::may_reopen`] and this answer are what a
/// caller uses to fall back to a playtest-fix chore instead.
#[derive(Debug, Clone, PartialEq)]
pub enum ReopenAnswer {
    Reopened { item: Box<Item>, token: String },
    Kept,
}

/// `chore_lock`'s answer (backlog/06-data-and-operations.md section 6.2, shared/backlogTypes.ts): a
/// project-wide named lock, used for grouping playtests under `TRYOUT_GROUPS_LOCK` as well as chores.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LockAnswer {
    pub held: bool,
    #[serde(default)]
    pub by: Option<String>,
}

/// Why an operation didn't answer. Every variant means the same thing to a caller: don't guess,
/// stop and wait for the store (agent/02-principles.md).
#[derive(Debug)]
pub enum BacklogError {
    /// The store couldn't be reached at all: no address, no network, a timeout, a refused connection.
    Unreachable(String),
    /// The store answered, but refused the request (`code` is its `error`, e.g. `bad_request` or
    /// `not_found`) or sent something this agent can't read (no `code`).
    BadAnswer {
        status: u16,
        code: Option<String>,
        message: String,
    },
}

impl BacklogError {
    /// The store's own refusal code, when it refused.
    pub fn code(&self) -> Option<&str> {
        match self {
            BacklogError::BadAnswer { code, .. } => code.as_deref(),
            BacklogError::Unreachable(_) => None,
        }
    }
}

impl std::fmt::Display for BacklogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BacklogError::Unreachable(msg) => write!(f, "couldn't reach the backlog store: {msg}"),
            BacklogError::BadAnswer { status, message, .. } => {
                write!(f, "the backlog store answered {status}: {message}")
            }
        }
    }
}

impl std::error::Error for BacklogError {}

#[derive(Serialize)]
struct OpRequest {
    op: &'static str,
    args: Value,
}

#[derive(Deserialize, Default)]
struct OpResponse {
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// A client for one backlog store (one `BacklogStore` in the TypeScript agent, trimmed to `list`).
pub struct Client {
    base_url: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("building the backlog HTTP client");
        Self {
            base_url: base_url.into(),
            http,
        }
    }

    /// `list` (backlog/06.2-operations.md): every item in the project's backlog, in rank order.
    /// `token` is this machine's access token for the store (the device-link flow that gets one isn't
    /// ported here yet); a request with none is sent as the store's own `unauthorized` answer, not
    /// guessed around.
    pub async fn list(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Item>, BacklogError> {
        self.op(repo_id, token, "list", json!({}), "items").await
    }

    /// `drafts` (backlog/06.2-operations.md): the project's spec drafts, oldest first.
    pub async fn drafts(&self, repo_id: u64, token: Option<&str>) -> Result<Vec<Draft>, BacklogError> {
        self.op(repo_id, token, "drafts", json!({}), "drafts").await
    }

    /// `claim_next` (backlog/06.6-handing-out-work.md): the next item ready for this agent, or
    /// `Nothing` while the project is paused or nothing is ready. Unlike `op`'s other callers, a
    /// `null` value is a real, successful answer here, not a bad one.
    pub async fn claim_next(
        &self,
        repo_id: u64,
        token: Option<&str>,
        agent: &ClaimNextAgent,
    ) -> Result<ClaimNextAnswer, BacklogError> {
        let (status, body) = self
            .send(repo_id, token, "claim_next", json!({ "agent": agent }))
            .await?;
        match body.value {
            None | Some(Value::Null) => Ok(ClaimNextAnswer::Nothing),
            Some(value) => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Answer {
                    item: Item,
                    token: String,
                }
                let answer: Answer = serde_json::from_value(value).map_err(|e| BacklogError::BadAnswer {
                    status,
                    code: None,
                    message: format!("couldn't read the item it claimed: {e}"),
                })?;
                Ok(ClaimNextAnswer::Item {
                    item: Box::new(answer.item),
                    token: answer.token,
                })
            }
        }
    }

    /// `release` (backlog/06.6-handing-out-work.md; `HostedWork.release` in the TS agent): gives the
    /// lease `lease` on item `id` back, so the store hands the item out again. A `stale_lease` refusal
    /// means the lease is gone already, which a caller treats as released.
    pub async fn release(&self, repo_id: u64, token: Option<&str>, id: &str, lease: &str) -> Result<(), BacklogError> {
        self.send(repo_id, token, "release", json!({ "id": id, "token": lease }))
            .await?;
        Ok(())
    }

    /// `report` (agent/07.5-what-a-claim-and-a-give-back-send.md): adds `note`, one plain sentence, to
    /// item `id`'s history, sent while the lease `lease` still holds, before a give-back's `release`.
    pub async fn report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        note: &str,
    ) -> Result<(), BacklogError> {
        self.send(
            repo_id,
            token,
            "report",
            json!({ "id": id, "token": lease, "note": note }),
        )
        .await?;
        Ok(())
    }

    /// `park` (`HostedWork.park` in the TS agent): lets go of the lease on item `id` while it waits for
    /// `waits_for` (`answer`, `help`, `ci`, `tryout` or `merge`), staying its owner so it isn't handed out
    /// to another computer. The item is claimed back by name when the wait ends here.
    pub async fn park(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        lease: &str,
        waits_for: &str,
    ) -> Result<(), BacklogError> {
        self.send(
            repo_id,
            token,
            "park",
            json!({ "id": id, "token": lease, "for": waits_for }),
        )
        .await?;
        Ok(())
    }

    /// `specs_commit` (Retaskable.com's backlog.md section 6.4): the commit the store last took Items headers
    /// from, or `None` before it took any.
    pub async fn specs_commit(&self, repo_id: u64, token: Option<&str>) -> Result<Option<String>, BacklogError> {
        let (status, body) = self.send(repo_id, token, "specs_commit", json!({})).await?;
        match body.value {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(commit)) => Ok(Some(commit)),
            Some(other) => Err(BacklogError::BadAnswer {
                status,
                code: None,
                message: format!("couldn't read the specs commit it sent: {other}"),
            }),
        }
    }

    /// `tryouts_list` (backlog/06-data-and-operations.md section 6.2): the shared after-merge
    /// try-out queue still waiting on a person, and the grouping last sent for it, if any.
    pub async fn tryouts_list(&self, repo_id: u64, token: Option<&str>) -> Result<TryoutsList, BacklogError> {
        self.op(repo_id, token, "tryouts_list", json!({}), "try-out queue")
            .await
    }

    /// `tryout_add` (backlog/06-data-and-operations.md section 6.2): adds (or, on a repeat, no-ops) one
    /// merged slice's after-merge try-out to the shared queue.
    pub async fn tryout_add(&self, repo_id: u64, token: Option<&str>, add: &TryoutAdd) -> Result<(), BacklogError> {
        self.send(repo_id, token, "tryout_add", json!(add)).await?;
        Ok(())
    }

    /// `tryout_result` (backlog/06-data-and-operations.md section 6.2): records a person's answer to
    /// the try-out named `key`, once.
    pub async fn tryout_result(
        &self,
        repo_id: u64,
        token: Option<&str>,
        key: &str,
        outcome: TryoutOutcome,
        notes: Option<&str>,
    ) -> Result<(), BacklogError> {
        self.send(
            repo_id,
            token,
            "tryout_result",
            json!({ "key": key, "result": outcome, "notes": notes }),
        )
        .await?;
        Ok(())
    }

    /// `tryout_groups` (docs/specs/agent/06.4-playtests.md "Grouped into sessions"): sends the
    /// planner's grouping of the waiting try-outs, under `signature` (its own, so a stale round
    /// never overwrites a newer one).
    pub async fn tryout_groups(
        &self,
        repo_id: u64,
        token: Option<&str>,
        signature: &str,
        groups: &[TryoutGroup],
    ) -> Result<(), BacklogError> {
        self.send(
            repo_id,
            token,
            "tryout_groups",
            json!({ "signature": signature, "groups": groups }),
        )
        .await?;
        Ok(())
    }

    /// `reopen` (docs/specs/agent/06.4-playtests.md "Problem reopens the item itself"): asks the
    /// store to put the item back in the queue with the try-out's notes as its next slice, and hand
    /// it straight back to this agent (`agent` is this computer's, the same as `claim_next`'s) so the
    /// session that built it can resume. `ReopenAnswer::Kept` when the store declined (the item's
    /// being built, or it's an older store that doesn't know `reopen`): the caller queues a
    /// playtest-fix chore with `req`'s notes and steps instead, as before.
    pub async fn reopen(
        &self,
        repo_id: u64,
        token: Option<&str>,
        req: &ReopenRequest<'_>,
        agent: &ClaimNextAgent,
    ) -> Result<ReopenAnswer, BacklogError> {
        let mut args = json!({ "id": req.item_id, "notes": req.notes, "agent": agent });
        let obj = args.as_object_mut().expect("built as an object above");
        if let Some(by) = req.by {
            obj.insert("by".to_string(), json!(by));
        }
        if let Some(what) = req.what {
            obj.insert("what".to_string(), json!(what));
        }
        if let Some(steps) = req.steps {
            obj.insert("steps".to_string(), json!(steps));
        }
        let (status, body) = self.send(repo_id, token, "reopen", args).await?;
        let bad = |message: String| BacklogError::BadAnswer {
            status,
            code: None,
            message,
        };
        let value = body.value.ok_or_else(|| bad("its answer had no value".to_string()))?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Answer {
            reopened: bool,
            #[serde(default)]
            item: Option<Item>,
            #[serde(default)]
            token: Option<String>,
        }
        let answer: Answer =
            serde_json::from_value(value).map_err(|e| bad(format!("couldn't read its reopen answer: {e}")))?;
        Ok(match (answer.reopened, answer.item, answer.token) {
            (true, Some(item), Some(token)) => ReopenAnswer::Reopened {
                item: Box::new(item),
                token,
            },
            _ => ReopenAnswer::Kept,
        })
    }

    /// `chore_lock` (backlog/06-data-and-operations.md section 6.2, shared/backlogTypes.ts): takes or
    /// drops the project-wide lock named `name`, so only one computer runs the work it guards at a
    /// time (the playtest planner's own lock name is
    /// [`crate::playtest_groups::TRYOUT_GROUPS_LOCK`]).
    pub async fn lock(
        &self,
        repo_id: u64,
        token: Option<&str>,
        name: &str,
        take: bool,
    ) -> Result<LockAnswer, BacklogError> {
        self.op(
            repo_id,
            token,
            "chore_lock",
            json!({ "name": name, "take": take }),
            "lock answer",
        )
        .await
    }

    /// One operation on the project's backlog: `POST <store>/v1/repos/<repo id>/ops` with `{ op, args }`,
    /// answering its `value` read as `T` (`what` names it when it can't be read). A refusal keeps the
    /// store's own `error` code (`bad_request`, `not_found`, ...) so a caller can tell them apart.
    pub async fn op<T: DeserializeOwned>(
        &self,
        repo_id: u64,
        token: Option<&str>,
        op: &'static str,
        args: Value,
        what: &str,
    ) -> Result<T, BacklogError> {
        let (status, body) = self.send(repo_id, token, op, args).await?;
        let bad = |message: String| BacklogError::BadAnswer {
            status,
            code: None,
            message,
        };
        let value = body.value.ok_or_else(|| bad("its answer had no value".to_string()))?;
        serde_json::from_value(value).map_err(|e| bad(format!("couldn't read the {what} it sent: {e}")))
    }

    /// Sends one `{ op, args }` request and reads back its status and body, every network or
    /// non-2xx failure turned into a `BacklogError` so no caller can mistake "couldn't ask" for an
    /// empty or missing answer.
    async fn send(
        &self,
        repo_id: u64,
        token: Option<&str>,
        op: &'static str,
        args: Value,
    ) -> Result<(u16, OpResponse), BacklogError> {
        let url = format!("{}/v1/repos/{}/ops", self.base_url, repo_id);
        let mut req = self.http.post(&url).json(&OpRequest { op, args });
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let res = req.send().await.map_err(|e| BacklogError::Unreachable(e.to_string()))?;
        let status = res.status();
        let body: OpResponse = res.json().await.unwrap_or_default();
        if !status.is_success() {
            let message = body
                .message
                .or_else(|| body.error.clone())
                .unwrap_or_else(|| format!("the store answered {status}"));
            return Err(BacklogError::BadAnswer {
                status: status.as_u16(),
                code: body.error,
                message,
            });
        }
        Ok((status.as_u16(), body))
    }
}

#[cfg(test)]
mod tests;
