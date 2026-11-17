//! **Publish** and **Unpublish** on the release note, run by the loop (`driver/release_page.rs`).
use axum::Router;
use axum::routing::put;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::super::asks::{Listed, store_failing};
use super::*;
use crate::link::TokenAnswer;
use crate::view::{CloudToken, Link, ProjectCommand};

struct Cloud(String);

impl CloudToken for Cloud {
    fn url(&self) -> &str {
        &self.0
    }

    fn answer(&self) -> BoxFuture<'_, TokenAnswer> {
        let (url, access_token) = (self.0.clone(), "tok".to_string());
        async move { TokenAnswer::Token { url, access_token } }.boxed()
    }
}

/// An agent cloud whose page answers every PUT with a URL and every DELETE with 204.
async fn own_cloud() -> String {
    let app = Router::new().route(
        "/api/agents/releases/{id}",
        put(|| async { axum::Json(json!({ "url": "https://x/p/page" })) })
            .delete(|| async { axum::http::StatusCode::NO_CONTENT }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn note_fact(extra: Value) -> Value {
    let mut note = json!({ "from": "a", "to": "b", "state": "ready", "markdown": "# New", "generatedAt": "2026-11-17T10:00:00.000Z" });
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        note[k] = v;
    }
    json!({ "release_note": { "data": { "note": note }, "updatedAt": 7 } })
}

async fn publishing(facts: Value, unpublish: bool) -> (Value, Listed) {
    let (base, _asked, listed) = store_failing("").await;
    *listed.facts.lock().unwrap() = Some(facts);
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.wanted = true;
    let items = rig.machine.links.item_commands.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    items.projects.push(ProjectCommand::Release {
        project: "p0".into(),
        command_id: "c1".into(),
        unpublish,
        link: Link(Arc::new(Cloud(own_cloud().await))),
    });
    rig.machine.pass().await;
    let answer = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    (answer, listed)
}

#[tokio::test]
async fn publish_answers_the_page_address_and_writes_the_page_into_the_stores_note() {
    let (answer, listed) = publishing(note_fact(json!({})), false).await;
    assert_eq!(
        answer,
        json!({ "type": "ack", "id": "c1", "result": { "url": "https://x/p/page" } })
    );
    let writes: Vec<Value> = (listed.fact_writes.lock().unwrap().iter())
        .filter(|w| w["kind"] == "release_note")
        .cloned()
        .collect();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0]["ifUpdatedAt"], 7);
    let note = &writes[0]["data"]["note"];
    assert_eq!(
        (note["markdown"].clone(), note["published"]["url"].clone()),
        (json!("# New"), json!("https://x/p/page"))
    );
}

#[tokio::test]
async fn unpublish_of_a_published_note_acks_and_clears_it_and_an_unpublished_one_is_refused() {
    let published =
        json!({ "published": { "id": "AbCdEfGhIjKl", "url": "https://x/p", "at": "t", "markdown": "# New" } });
    let (answer, listed) = publishing(note_fact(published), true).await;
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    let writes = (listed.fact_writes.lock().unwrap().iter())
        .filter(|w| w["kind"] == "release_note")
        .cloned()
        .collect::<Vec<_>>();
    assert!(writes[0]["data"]["note"].get("published").is_none());
    let (answer, listed) = publishing(note_fact(json!({})), true).await;
    assert_eq!(
        (answer["type"].as_str(), answer["reason"].as_str()),
        (Some("nack"), Some("The note isn't published."))
    );
    assert!(
        listed
            .fact_writes
            .lock()
            .unwrap()
            .iter()
            .all(|w| w["kind"] != "release_note")
    );
}

#[tokio::test]
async fn with_no_note_in_the_store_nothing_is_published() {
    let (answer, _) = publishing(json!({}), false).await;
    assert_eq!(answer["reason"], "There's no note ready to publish.");
}
