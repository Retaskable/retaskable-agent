//! The agent's side of the shared backlog store (docs/specs/agent/02-principles.md: "The store is
//! the source of truth... When the store can't be reached it stops and waits: it fails closed, and
//! never falls back to a local list."): `list`, read over `POST <store>/v1/repos/<repo id>/ops`
//! (Retaskable.com's docs/specs/backlog.md sections 3.1 and 6.2). A repo's numeric GitHub id and
//! this machine's access token come from elsewhere (device linking isn't ported here yet); every
//! way this can fail — no address, no network, a refused or unreadable answer — comes back as a
//! `BacklogError` and never as a guessed or empty list, so a caller can't mistake "couldn't ask" for
//! "nothing to do".
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// Why `list` didn't answer the items. Every variant means the same thing to a caller: don't guess,
/// stop and wait for the store (agent/02-principles.md).
#[derive(Debug)]
pub enum BacklogError {
    /// The store couldn't be reached at all: no address, no network, a timeout, a refused connection.
    Unreachable(String),
    /// The store answered, but refused the request or sent something this agent can't read.
    BadAnswer { status: u16, message: String },
}

impl std::fmt::Display for BacklogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BacklogError::Unreachable(msg) => write!(f, "couldn't reach the backlog store: {msg}"),
            BacklogError::BadAnswer { status, message } => {
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
        let url = format!("{}/v1/repos/{}/ops", self.base_url, repo_id);
        let mut req = self.http.post(&url).json(&OpRequest {
            op: "list",
            args: Value::Object(Default::default()),
        });
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let res = req.send().await.map_err(|e| BacklogError::Unreachable(e.to_string()))?;
        let status = res.status();
        let body: OpResponse = res.json().await.unwrap_or_default();
        if !status.is_success() {
            let message = body
                .message
                .or(body.error)
                .unwrap_or_else(|| format!("the store answered {status}"));
            return Err(BacklogError::BadAnswer {
                status: status.as_u16(),
                message,
            });
        }
        let value = body.value.ok_or_else(|| BacklogError::BadAnswer {
            status: status.as_u16(),
            message: "its answer had no value".to_string(),
        })?;
        serde_json::from_value(value).map_err(|e| BacklogError::BadAnswer {
            status: status.as_u16(),
            message: format!("couldn't read the items it sent: {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::post};
    use serde_json::json;
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
            BacklogError::BadAnswer { status, message } => {
                assert_eq!(status, 403);
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
}
