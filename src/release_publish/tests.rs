use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::put;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::*;
use crate::backlog::BacklogError;

const NOW: i64 = 1_800_000_000_000;

/// The store's `release_note` row, with its version.
#[derive(Default)]
struct Rows(Mutex<BTreeMap<&'static str, (Value, u64)>>);

impl FactStore for Rows {
    fn get(&self, kind: &'static str) -> BoxFuture<'_, Result<Option<(Value, u64)>, BacklogError>> {
        async move { Ok(self.0.lock().unwrap().get(kind).cloned()) }.boxed()
    }

    fn set(&self, kind: &'static str, data: Value, at: Option<u64>) -> BoxFuture<'_, Result<(), BacklogError>> {
        async move {
            let mut rows = self.0.lock().unwrap();
            let now = rows.get(kind).map_or(0, |r| r.1);
            assert!(at.is_none_or(|a| a == now), "written over the version it read");
            rows.insert(kind, (data, now + 1));
            Ok(())
        }
        .boxed()
    }
}

fn rows(note: Value) -> Rows {
    let rows = Rows::default();
    rows.0
        .lock()
        .unwrap()
        .insert(RELEASE_NOTE, (json!({ "note": note }), 1));
    rows
}

fn ready() -> Value {
    json!({ "from": "2026-11-01T00:00:00.000Z", "to": "2026-11-08T00:00:00.000Z", "state": "ready",
        "markdown": "# What's new", "generatedAt": iso_now(NOW - 60_000), "edited": false })
}

fn note_of(rows: &Rows) -> Value {
    rows.0.lock().unwrap()[RELEASE_NOTE].0["note"].clone()
}

struct Link(Mutex<Option<TokenAnswer>>);

impl CloudToken for Link {
    fn url(&self) -> &str {
        "cloud"
    }

    fn answer(&self) -> BoxFuture<'_, TokenAnswer> {
        async move { self.0.lock().unwrap().clone().unwrap() }.boxed()
    }
}

/// What the cloud was asked: method, page id, project, bearer token and body.
type Asked = Arc<Mutex<Vec<(String, String, Option<String>, String, Value)>>>;

/// A cloud whose pages answer `status` with `body`; every request is kept.
async fn cloud(status: u16, body: Value) -> (String, Asked) {
    let asked: Asked = Default::default();
    async fn handle(
        State((asked, status, body)): State<(Asked, u16, Value)>,
        method: axum::http::Method,
        Path(id): Path<String>,
        Query(q): Query<BTreeMap<String, String>>,
        headers: HeaderMap,
        text: String,
    ) -> impl IntoResponse {
        let bearer = headers["authorization"].to_str().unwrap().to_string();
        let sent = serde_json::from_str(&text).unwrap_or(Value::Null);
        asked
            .lock()
            .unwrap()
            .push((method.to_string(), id, q.get("project").cloned(), bearer, sent));
        (StatusCode::from_u16(status).unwrap(), axum::Json(body))
    }
    let app = Router::new()
        .route("/api/agents/releases/{id}", put(handle).delete(handle))
        .with_state((asked.clone(), status, body));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn linked(base: &str) -> Link {
    Link(Mutex::new(Some(TokenAnswer::Token {
        url: base.to_string(),
        access_token: "tok".into(),
    })))
}

async fn publishing(rows: &Rows, link: &Link, project: &str) -> Result<Value, String> {
    let facts = KeptFacts::default();
    publish(rows, &facts, project, link, &reqwest::Client::new(), NOW).await
}

#[tokio::test]
async fn a_ready_note_is_put_on_a_new_page_and_the_note_remembers_it_and_the_next_publish_updates_it() {
    let (base, asked) = cloud(200, json!({ "url": "https://x/p/page" })).await;
    let store = rows(ready());
    let link = linked(&base);
    assert_eq!(
        publishing(&store, &link, "proj one").await,
        Ok(json!({ "url": "https://x/p/page" }))
    );
    let published = note_of(&store)["published"].clone();
    let id = published["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 12);
    assert!(id.bytes().all(|b| b.is_ascii_alphanumeric()));
    assert_eq!(
        (published["url"].clone(), published["markdown"].clone()),
        (json!("https://x/p/page"), json!("# What's new"))
    );
    assert_eq!(note_of(&store)["markdown"], "# What's new");
    {
        let sent = asked.lock().unwrap();
        assert_eq!(
            (
                &sent[0].0[..],
                sent[0].1.as_str(),
                sent[0].2.as_deref(),
                sent[0].3.as_str()
            ),
            ("PUT", id.as_str(), Some("proj one"), "Bearer tok")
        );
        assert_eq!(
            sent[0].4,
            json!({ "markdown": "# What's new", "from": "2026-11-01T00:00:00.000Z", "to": "2026-11-08T00:00:00.000Z" })
        );
    }
    publishing(&store, &link, "proj one").await.unwrap();
    assert_eq!(asked.lock().unwrap()[1].1, id, "Update the public page: the same id");
}

#[tokio::test]
async fn a_note_that_is_not_ready_or_is_too_long_or_is_already_publishing_is_refused_in_ts_words() {
    let link = linked("http://127.0.0.1:1");
    let refused = |note: Value| async move {
        publishing(&rows(note), &linked("http://127.0.0.1:1"), "p")
            .await
            .unwrap_err()
    };
    for state in ["gathering", "writing"] {
        let mut n = ready();
        n["state"] = json!(state);
        assert_eq!(refused(n).await, "Wait for the note to finish.");
    }
    // A build that began over 30 minutes ago is lost, so it is no note ready.
    let mut stale = ready();
    stale["state"] = json!("writing");
    stale["generatedAt"] = json!(iso_now(NOW - 31 * 60_000));
    assert_eq!(refused(stale).await, "There's no note ready to publish.");
    let mut failed = ready();
    failed["state"] = json!("failed");
    assert_eq!(refused(failed).await, "There's no note ready to publish.");
    let mut blank = ready();
    blank["markdown"] = json!("  \n");
    assert_eq!(refused(blank).await, "There's no note ready to publish.");
    let mut long = ready();
    long["markdown"] = json!("x".repeat(MAX_PUBLISHED_CHARS + 1));
    assert_eq!(
        refused(long).await,
        "A note can be at most 200,000 characters to publish."
    );
    let none = Rows::default();
    assert_eq!(
        publishing(&none, &link, "p").await.unwrap_err(),
        "There's no note ready to publish."
    );
    let turn = Turn::take("busy-project").unwrap();
    assert_eq!(
        publishing(&rows(ready()), &link, "busy-project").await.unwrap_err(),
        "It's already being published."
    );
    drop(turn);
}

#[tokio::test]
async fn an_agent_with_no_cloud_link_or_no_way_to_it_says_so_and_a_refusing_cloud_is_quoted() {
    let store = rows(ready());
    let idle = Link(Mutex::new(Some(TokenAnswer::Idle)));
    assert_eq!(
        publishing(&store, &idle, "idle-p").await.unwrap_err(),
        "Link this agent to retaskable.com first, in Profile."
    );
    let down = Link(Mutex::new(Some(TokenAnswer::Unreachable)));
    assert_eq!(
        publishing(&store, &down, "idle-p").await.unwrap_err(),
        "Couldn't reach retaskable.com: try again in a minute."
    );
    let (base, _) = cloud(402, json!({ "error": "Needs a plan" })).await;
    assert_eq!(
        publishing(&store, &linked(&base), "idle-p").await.unwrap_err(),
        "Couldn't publish it: 402 Needs a plan"
    );
    let (base, _) = cloud(200, json!({})).await;
    assert_eq!(
        publishing(&store, &linked(&base), "idle-p").await.unwrap_err(),
        "Couldn't publish it: retaskable.com didn't answer the page's address"
    );
    assert!(
        note_of(&store).get("published").is_none(),
        "nothing is written for a failed publish"
    );
    // The turn is given back whatever came of it.
    assert!(Turn::take("idle-p").is_some());
}

#[tokio::test]
async fn unpublish_deletes_the_page_and_the_note_forgets_it() {
    let (base, asked) = cloud(204, Value::Null).await;
    let mut note = ready();
    note["published"] = json!({ "id": "AbCdEfGhIjKl", "url": "https://x/p", "at": "t", "markdown": "# What's new" });
    let store = rows(note);
    let facts = KeptFacts::default();
    unpublish(&store, &facts, "p", &linked(&base), &reqwest::Client::new(), NOW)
        .await
        .unwrap();
    let sent = asked.lock().unwrap();
    assert_eq!(
        (&sent[0].0[..], sent[0].1.as_str(), sent[0].3.as_str()),
        ("DELETE", "AbCdEfGhIjKl", "Bearer tok")
    );
    assert!(note_of(&store).get("published").is_none());
    assert_eq!(note_of(&store)["markdown"], "# What's new");
}

#[tokio::test]
async fn unpublish_of_a_note_that_is_not_published_or_a_cloud_that_refuses_changes_nothing() {
    let store = rows(ready());
    let facts = KeptFacts::default();
    let http = reqwest::Client::new();
    let (base, _) = cloud(500, json!({ "error": "boom" })).await;
    assert_eq!(
        unpublish(&store, &facts, "p", &linked(&base), &http, NOW)
            .await
            .unwrap_err(),
        "The note isn't published."
    );
    let mut note = ready();
    note["published"] = json!({ "id": "AbCdEfGhIjKl", "url": "https://x/p", "at": "t", "markdown": "m" });
    let store = rows(note);
    assert_eq!(
        unpublish(&store, &facts, "p", &linked(&base), &http, NOW)
            .await
            .unwrap_err(),
        "Couldn't unpublish it: 500 boom"
    );
    assert!(note_of(&store).get("published").is_some());
}
