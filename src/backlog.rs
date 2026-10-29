//! The agent's side of the shared backlog store (docs/specs/agent/02-principles.md: "The store is
//! the source of truth... When the store can't be reached it stops and waits: it fails closed, and
//! never falls back to a local list."): `list`, `claim_next`, `drafts`, the after-merge try-out
//! queue (`tryout_add`, `tryout_result`, `tryout_groups`, docs/specs/agent/06.4-playtests.md) and a
//! named project-wide lock (`chore_lock`), each read over `POST <store>/v1/repos/<repo id>/ops`
//! (Retaskable.com's docs/specs/backlog.md sections 3.1 and 6.2, and backlog/06.6-handing-out-work.md
//! for `claim_next`), and `auto_link`, this machine's own link to the store
//! (docs/specs/installers/04-every-agent-on-the-shared-backlog-shared.md). A repo's numeric GitHub
//! id, and wiring a linked access token in, come from elsewhere (not ported here yet); every way an
//! operation can fail — no address, no network, a refused or unreadable answer — comes back as a
//! `BacklogError` and never as a guessed or empty answer, so a caller can't mistake "couldn't ask"
//! for "nothing to do".
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
pub fn url(env: impl Fn(&str) -> Option<String>) -> String {
    let raw = env("RETASKABLE_BACKLOG_URL")
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
    env: impl Fn(&str) -> Option<String>,
    version: &str,
    on_code: impl FnOnce(&DeviceCode),
) -> Result<Option<u64>> {
    let backlog_url = url(&env);
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
        machine: link::machine_info(&env, version),
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
/// machine name, the host of the retaskable.com it links to, and the project's base branch.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimNextAgent {
    pub machine: String,
    pub environment: String,
    pub base_branch: String,
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
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use std::net::TcpListener as StdTcpListener;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn url_defaults_then_takes_the_env_and_trims_a_trailing_slash() {
        assert_eq!(url(no_env), DEFAULT_URL);
        let env = |k: &str| (k == "RETASKABLE_BACKLOG_URL").then(|| "https://example.test/".to_string());
        assert_eq!(url(env), "https://example.test");
        let blank = |k: &str| (k == "RETASKABLE_BACKLOG_URL").then(|| "   ".to_string());
        assert_eq!(url(blank), DEFAULT_URL);
    }

    /// Starts a fake store answering `handler` on `POST /v1/repos/:id/ops`, and gives its base URL.
    async fn fake_store(handler: axum::routing::MethodRouter) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/repos/{id}/ops", handler);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn list_reads_the_items_the_store_answers() {
        let base = fake_store(post(|| async {
            Json(json!({
                "value": [
                    { "id": "a", "title": "First", "status": "todo", "rank": 0, "doNext": true,
                      "dependsOn": ["b"], "specRefs": ["docs/specs/x.md"], "slice": 1, "remaining": "more" },
                    { "id": "b", "title": "Second", "status": "on_main", "rank": 1 },
                ]
            }))
        }))
        .await;

        let items = Client::new(base).list(1, Some("tok")).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "a");
        assert_eq!(items[0].status, ItemStatus::Todo);
        assert!(items[0].do_next);
        assert_eq!(items[0].depends_on, vec!["b".to_string()]);
        assert_eq!(items[0].slice, 1);
        assert_eq!(items[0].remaining, Some("more".to_string()));
        // Fields the store left out take their defaults rather than failing to parse.
        assert_eq!(items[1].id, "b");
        assert_eq!(items[1].status, ItemStatus::OnMain);
        assert!(!items[1].do_next);
        assert!(items[1].depends_on.is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_store_is_its_own_error_not_an_empty_list() {
        // Bind, then drop the listener at once: nothing answers that port, like a down store.
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let err = Client::new(format!("http://{addr}")).list(1, None).await.unwrap_err();
        assert!(
            matches!(err, BacklogError::Unreachable(_)),
            "expected Unreachable, got {err:?}"
        );
    }

    #[tokio::test]
    async fn an_unaddressable_url_is_unreachable_too() {
        let err = Client::new("not a url").list(1, None).await.unwrap_err();
        assert!(
            matches!(err, BacklogError::Unreachable(_)),
            "expected Unreachable, got {err:?}"
        );
    }

    #[tokio::test]
    async fn drafts_sends_its_op_and_reads_each_drafts_pr() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["op"], "drafts");
            Json(json!({
                "value": [
                    { "kind": "draft", "id": "draft-1", "status": "merged", "titles": ["Charts"],
                      "prUrl": "https://github.com/x/y/pull/4" },
                    { "kind": "draft", "id": "draft-2", "status": "queued", "titles": [], "prUrl": null },
                ]
            }))
        }))
        .await;

        let drafts = Client::new(base).drafts(1, Some("tok")).await.unwrap();
        assert_eq!(drafts[0].pr_url.as_deref(), Some("https://github.com/x/y/pull/4"));
        assert_eq!(drafts[0].titles, ["Charts"]);
        assert_eq!(drafts[1].pr_url, None);
    }

    #[tokio::test]
    async fn claim_next_sends_the_agent_and_reads_back_the_claimed_item() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["op"], "claim_next");
            assert_eq!(req["args"]["agent"]["machine"], "box-1");
            assert_eq!(req["args"]["agent"]["baseBranch"], "main");
            Json(json!({
                "value": {
                    "item": { "id": "a", "title": "First", "status": "claimed", "rank": 0 },
                    "token": "lease-1",
                }
            }))
        }))
        .await;

        let agent = ClaimNextAgent {
            machine: "box-1".to_string(),
            environment: "Production".to_string(),
            base_branch: "main".to_string(),
        };
        let answer = Client::new(base).claim_next(1, Some("tok"), &agent).await.unwrap();
        match answer {
            ClaimNextAnswer::Item { item, token } => {
                assert_eq!(item.id, "a");
                assert_eq!(token, "lease-1");
            }
            ClaimNextAnswer::Nothing => panic!("expected an item"),
        }
    }

    #[tokio::test]
    async fn claim_next_reads_a_null_value_as_nothing_not_a_bad_answer() {
        let base = fake_store(post(|| async { Json(json!({ "value": null })) })).await;

        let agent = ClaimNextAgent {
            machine: "box-1".to_string(),
            environment: "Production".to_string(),
            base_branch: "main".to_string(),
        };
        let answer = Client::new(base).claim_next(1, Some("tok"), &agent).await.unwrap();
        assert_eq!(answer, ClaimNextAnswer::Nothing);
    }

    #[tokio::test]
    async fn tryout_add_sends_the_merged_slice_with_its_machine() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["op"], "tryout_add");
            assert_eq!(req["args"]["itemId"], "a");
            assert_eq!(req["args"]["mergedAt"], "2026-10-28T00:00:00Z");
            assert_eq!(req["args"]["title"], "Charts");
            assert_eq!(req["args"]["prUrl"], "https://github.com/x/y/pull/4");
            assert_eq!(req["args"]["machine"], "box-1");
            Json(json!({}))
        }))
        .await;

        let add = TryoutAdd {
            item_id: "a".to_string(),
            merged_at: "2026-10-28T00:00:00Z".to_string(),
            title: "Charts".to_string(),
            what: "Open the charts page".to_string(),
            steps: vec!["Open it".to_string()],
            pr_url: Some("https://github.com/x/y/pull/4".to_string()),
            machine: "box-1".to_string(),
        };
        Client::new(base).tryout_add(1, Some("tok"), &add).await.unwrap();
    }

    #[tokio::test]
    async fn tryout_add_leaves_out_pr_url_when_there_is_none() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert!(req["args"].as_object().unwrap().get("prUrl").is_none());
            Json(json!({}))
        }))
        .await;

        let add = TryoutAdd {
            item_id: "a".to_string(),
            merged_at: "2026-10-28T00:00:00Z".to_string(),
            title: "Charts".to_string(),
            what: "Open the charts page".to_string(),
            steps: vec![],
            pr_url: None,
            machine: "box-1".to_string(),
        };
        Client::new(base).tryout_add(1, None, &add).await.unwrap();
    }

    #[tokio::test]
    async fn tryout_result_sends_the_key_outcome_and_notes() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["op"], "tryout_result");
            assert_eq!(req["args"]["key"], "a@2026-10-28T00:00:00Z");
            assert_eq!(req["args"]["result"], "problem");
            assert_eq!(req["args"]["notes"], "The chart didn't load.");
            Json(json!({}))
        }))
        .await;

        Client::new(base)
            .tryout_result(
                1,
                Some("tok"),
                "a@2026-10-28T00:00:00Z",
                TryoutOutcome::Problem,
                Some("The chart didn't load."),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn tryout_groups_sends_its_signature_and_groups() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["op"], "tryout_groups");
            assert_eq!(req["args"]["signature"], "sig-1");
            assert_eq!(req["args"]["groups"][0]["title"], "Build");
            assert_eq!(req["args"]["groups"][0]["keys"][0], "a");
            Json(json!({}))
        }))
        .await;

        let groups = vec![TryoutGroup {
            title: "Build".to_string(),
            why: "Same place".to_string(),
            keys: vec!["a".to_string()],
        }];
        Client::new(base)
            .tryout_groups(1, Some("tok"), "sig-1", &groups)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn lock_sends_the_name_and_take_and_reads_who_holds_it() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["op"], "chore_lock");
            assert_eq!(req["args"]["name"], "tryout-groups");
            assert_eq!(req["args"]["take"], true);
            Json(json!({ "value": { "held": true, "by": "box-1" } }))
        }))
        .await;

        let answer = Client::new(base)
            .lock(1, Some("tok"), "tryout-groups", true)
            .await
            .unwrap();
        assert_eq!(
            answer,
            LockAnswer {
                held: true,
                by: Some("box-1".to_string())
            }
        );
    }

    #[tokio::test]
    async fn lock_dropping_sends_take_false_and_reads_a_lock_held_by_no_one() {
        let base = fake_store(post(|Json(req): Json<Value>| async move {
            assert_eq!(req["args"]["take"], false);
            Json(json!({ "value": { "held": false } }))
        }))
        .await;

        let answer = Client::new(base)
            .lock(1, Some("tok"), "tryout-groups", false)
            .await
            .unwrap();
        assert_eq!(answer, LockAnswer { held: false, by: None });
    }

    #[tokio::test]
    async fn a_refusal_is_a_bad_answer_with_its_status_and_message() {
        let base = fake_store(post(|| async {
            (
                axum::http::StatusCode::FORBIDDEN,
                Json(json!({ "error": "forbidden", "message": "No access to this repo." })),
            )
        }))
        .await;

        let err = Client::new(base).list(1, Some("tok")).await.unwrap_err();
        match err {
            BacklogError::BadAnswer { status, code, message } => {
                assert_eq!(status, 403);
                assert_eq!(code.as_deref(), Some("forbidden"));
                assert_eq!(message, "No access to this repo.");
            }
            other => panic!("expected BadAnswer, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_success_status_with_no_value_is_a_bad_answer_not_an_empty_list() {
        let base = fake_store(post(|| async { Json(json!({})) })).await;

        let err = Client::new(base).list(1, None).await.unwrap_err();
        assert!(
            matches!(err, BacklogError::BadAnswer { status: 200, .. }),
            "expected BadAnswer, got {err:?}"
        );
    }

    #[tokio::test]
    async fn items_that_dont_parse_are_a_bad_answer() {
        let base = fake_store(post(|| async { Json(json!({ "value": [{ "id": "a" }] })) })).await;

        let err = Client::new(base).list(1, None).await.unwrap_err();
        assert!(
            matches!(err, BacklogError::BadAnswer { status: 200, .. }),
            "expected BadAnswer, got {err:?}"
        );
    }

    fn file_store() -> (tempfile::TempDir, Arc<dyn TokenStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::link::FileStore::new(
            dir.path().join("credentials.json"),
            "retaskable",
        ));
        (dir, store)
    }

    /// Starts a fake store answering the device flow on `/v1/device/code` and `/v1/device/token`, and gives
    /// its base URL.
    async fn fake_device_store(code: Value, tokens: Vec<Value>) -> String {
        use std::sync::Mutex as StdMutex;
        struct Fake {
            code: Value,
            tokens: StdMutex<std::collections::VecDeque<Value>>,
        }
        let fake = Arc::new(Fake {
            code,
            tokens: StdMutex::new(tokens.into()),
        });
        async fn code_route(axum::extract::State(f): axum::extract::State<Arc<Fake>>) -> Json<Value> {
            Json(f.code.clone())
        }
        async fn token_route(axum::extract::State(f): axum::extract::State<Arc<Fake>>) -> Json<Value> {
            Json(f.tokens.lock().unwrap().pop_front().expect("an answer for /token"))
        }
        let app = Router::new()
            .route("/v1/device/code", post(code_route))
            .route("/v1/device/token", post(token_route))
            .with_state(fake);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn auto_link_starts_the_device_flow_against_v1_device_and_stores_the_refresh_token() {
        let base = fake_device_store(
            json!({ "device_code": "dc", "user_code": "WXYZ-2345", "verification_uri": "https://x/link",
                    "verification_uri_complete": "https://x/link?code=WXYZ-2345", "interval": 1, "expires_in": 900 }),
            vec![json!({ "refresh_token": "r1", "agent_id": 9 })],
        )
        .await;
        let env = {
            let base = base.clone();
            move |k: &str| (k == "RETASKABLE_BACKLOG_URL").then(|| base.clone())
        };
        let (_dir, store) = file_store();
        let mut shown = None;
        let id = auto_link(store.clone(), reqwest::Client::new(), env, "9.9.9", |c| {
            shown = Some(c.clone())
        })
        .await
        .unwrap();
        assert_eq!(id, Some(9));
        assert_eq!(shown.unwrap().user_code, "WXYZ-2345");
        assert_eq!(store.get(&base).unwrap().as_deref(), Some("r1"));
    }

    #[tokio::test]
    async fn auto_link_does_nothing_when_already_linked() {
        // An address nothing listens on: a request here would fail, proving none was sent.
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let base = format!("http://{addr}");
        let env = {
            let base = base.clone();
            move |k: &str| (k == "RETASKABLE_BACKLOG_URL").then(|| base.clone())
        };
        let (_dir, store) = file_store();
        store.set(&base, "already-linked").unwrap();
        let id = auto_link(store, reqwest::Client::new(), env, "9.9.9", |_| {
            panic!("no code expected")
        })
        .await
        .unwrap();
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn auto_link_does_nothing_for_a_bad_backlog_url() {
        let env = |k: &str| (k == "RETASKABLE_BACKLOG_URL").then(|| "not a url".to_string());
        let (_dir, store) = file_store();
        let id = auto_link(store, reqwest::Client::new(), env, "9.9.9", |_| {
            panic!("no code expected")
        })
        .await
        .unwrap();
        assert_eq!(id, None);
    }
}
