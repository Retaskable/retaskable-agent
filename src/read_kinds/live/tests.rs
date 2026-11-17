use axum::Router;
use axum::http::StatusCode;
use axum::response::Redirect;
use axum::routing::get;
use serde_json::json;

use super::*;
use crate::config::{DeployConfig, DeployTarget};

#[test]
fn a_live_address_is_a_web_address_on_a_named_host_and_a_bare_site_is_its_origin() {
    assert_eq!(
        live_address("you.github.io/your-site").unwrap(),
        "https://you.github.io/your-site"
    );
    assert_eq!(live_address("  my.site  ").unwrap(), "https://my.site");
    assert_eq!(live_address("https://my.site/").unwrap(), "https://my.site");
    assert_eq!(
        live_address("http://my.site:8080/a?x=1").unwrap(),
        "http://my.site:8080/a?x=1"
    );
    assert_eq!(live_address("https://my.site/?").unwrap(), "https://my.site");
    assert_eq!(live_address("https://my.site/#top").unwrap(), "https://my.site");
    for bad in [
        "",
        "  ",
        "localhost",
        "http://localhost:3000",
        "ftp://my.site",
        "my site.com",
        "my.site:8080",
        "javascript:alert(1).x",
    ] {
        assert_eq!(live_address(bad), None, "{bad}");
    }
}

async fn site() -> String {
    let app = Router::new()
        .route("/ok", get(|| async { "hi" }))
        .route("/moved", get(|| async { Redirect::temporary("/ok") }))
        .route("/missing", get(|| async { StatusCode::NOT_FOUND }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

fn cx(base_branch: &str, production: &str) -> Reading {
    let mut cx = crate::read_kinds::tests::reading("/nowhere", "http://127.0.0.1:1");
    cx.project.base_branch = base_branch.into();
    if !production.is_empty() {
        cx.project.deploy = DeployConfig {
            production: Some(DeployTarget {
                branch: production.into(),
                ..serde_json::from_value(json!({})).unwrap()
            }),
            ..Default::default()
        };
    }
    cx
}

#[tokio::test]
async fn an_address_that_answers_after_redirects_is_the_url_to_save() {
    let base = site().await;
    let got = go_live(&cx("main", ""), &json!({ "url": format!("{base}/moved") }))
        .await
        .unwrap();
    assert_eq!(got, json!({ "url": format!("{base}/moved") }));
}

#[tokio::test]
async fn an_address_that_does_not_answer_is_refused_in_ts_words() {
    let base = site().await;
    let url = format!("{base}/missing");
    let why = go_live(&cx("main", ""), &json!({ "url": url })).await.unwrap_err();
    assert_eq!(
        why,
        ReadError::Refused(format!(
            "{url} answered 404. If the provider is still building it, wait a minute and check again."
        ))
    );
    let down = go_live(&cx("main", ""), &json!({ "url": "http://127.0.0.1:1/" }))
        .await
        .unwrap_err();
    assert!(
        down.why().starts_with("Couldn't reach http://127.0.0.1:1")
            && down.why().ends_with("wait a minute and check again."),
        "{}",
        down.why()
    );
}

#[tokio::test]
async fn what_is_not_a_web_address_or_names_a_branch_with_spaces_is_refused_before_any_request() {
    let nope = go_live(&cx("main", ""), &json!({ "url": "localhost" }))
        .await
        .unwrap_err();
    assert_eq!(
        nope.why(),
        "That isn't a web address. Copy the one the provider shows, like https://you.github.io/your-site."
    );
    let spaced = go_live(&cx("main", ""), &json!({ "url": "a.b", "branch": "my branch" }))
        .await
        .unwrap_err();
    assert_eq!(spaced.why(), "A branch name has no spaces.");
    // The project's own production branch, or else its base, is what stands when none is named.
    let configured = go_live(&cx("main", "gh pages"), &json!({ "url": "a.b" }))
        .await
        .unwrap_err();
    assert_eq!(configured.why(), "A branch name has no spaces.");
}

#[tokio::test]
async fn confirm_asks_the_store_to_process_the_item_and_says_the_stores_words_when_it_refuses() {
    use axum::{Json, extract::State};
    use std::sync::{Arc, Mutex};
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Default::default();
    async fn ops(
        State(seen): State<Arc<Mutex<Vec<serde_json::Value>>>>,
        Json(body): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        seen.lock().unwrap().push(body.clone());
        if body["args"]["id"] == "held-no-more" {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "conflict", "message": "It isn't waiting to be queued." })),
            );
        }
        (StatusCode::OK, Json(json!({ "value": { "id": body["args"]["id"] } })))
    }
    let app = Router::new()
        .route("/v1/repos/{id}/ops", axum::routing::post(ops))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cx = crate::read_kinds::tests::reading("/nowhere", &base);
    assert_eq!(confirm(&cx, &json!({ "itemId": " a-1 " })).await, Ok(Value::Null));
    assert_eq!(seen.lock().unwrap()[0]["op"], "process");
    assert_eq!(seen.lock().unwrap()[0]["args"], json!({ "id": "a-1" }));
    assert_eq!(
        confirm(&cx, &json!({ "itemId": "held-no-more" })).await,
        Err(ReadError::Refused("It isn't waiting to be queued.".into()))
    );
    assert_eq!(
        confirm(&cx, &json!({})).await.unwrap_err().why(),
        "Process needs the item's id."
    );
    let gone = crate::read_kinds::tests::reading("/nowhere", "http://127.0.0.1:1");
    assert!(matches!(
        confirm(&gone, &json!({ "itemId": "a" })).await,
        Err(ReadError::Failed(_))
    ));
}
