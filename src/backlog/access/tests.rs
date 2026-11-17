//! A fake store that takes only the access token it issued as a bearer, like the real one (devices.ts
//! `verifyAccessToken`): a refresh token sent as a bearer is a 401.
use super::*;
use crate::backlog::Client;
use crate::link::{BACKLOG_TARGET, FileStore, TokenStore};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Fake {
    /// What `/v1/device/token` answers, in turn.
    refreshes: Mutex<VecDeque<(StatusCode, Value)>>,
    /// The refresh tokens it was shown.
    shown: Mutex<Vec<String>>,
    /// The access tokens the ops accept.
    accepts: Mutex<Vec<String>>,
    /// Every bearer an op carried.
    bearers: Mutex<Vec<String>>,
}

fn issued(access: &str, refresh: &str) -> (StatusCode, Value) {
    (
        StatusCode::OK,
        json!({ "access_token": access, "refresh_token": refresh, "expires_in": 3600 }),
    )
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = Router::new()
        .route(
            "/v1/device/token",
            post(|State(f): State<Arc<Fake>>, Json(body): Json<Value>| async move {
                f.shown
                    .lock()
                    .unwrap()
                    .push(body["refresh_token"].as_str().unwrap_or_default().to_string());
                let (status, json) = f.refreshes.lock().unwrap().pop_front().expect("an unexpected refresh");
                (status, Json(json))
            }),
        )
        .route(
            "/v1/repos/{id}/ops",
            post(|State(f): State<Arc<Fake>>, headers: HeaderMap| async move {
                let bearer = headers
                    .get("authorization")
                    .map_or("", |h| h.to_str().unwrap())
                    .trim_start_matches("Bearer ")
                    .to_string();
                f.bearers.lock().unwrap().push(bearer.clone());
                if f.accepts.lock().unwrap().contains(&bearer) {
                    (StatusCode::OK, Json(json!({ "value": [] })))
                } else {
                    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })))
                }
            }),
        )
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// A client on the fake, its tokens and the store holding the refresh token `r1`.
async fn rig(
    fake: &Arc<Fake>,
) -> (
    Client,
    Arc<DeviceTokens>,
    Arc<dyn TokenStore>,
    String,
    tempfile::TempDir,
) {
    let url = serve(fake.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TokenStore> = Arc::new(FileStore::new(dir.path().join("credentials.json"), "retaskable"));
    store.set(&url, "r1").unwrap();
    let tokens = Arc::new(DeviceTokens::new(
        url.clone(),
        BACKLOG_TARGET,
        store.clone(),
        reqwest::Client::new(),
    ));
    let client = Client::with_access(url.clone(), tokens.clone());
    (client, tokens, store, url, dir)
}

#[tokio::test]
async fn a_store_call_carries_the_access_token_never_the_refresh_token() {
    let fake = Arc::new(Fake::default());
    fake.refreshes.lock().unwrap().push_back(issued("a1", "r2"));
    fake.accepts.lock().unwrap().push("a1".into());
    let (client, tokens, store, url, _dir) = rig(&fake).await;
    let token = current(&*tokens).await.unwrap().expect("linked");
    assert_eq!(token, "a1");
    assert!(client.list(1, Some(&token)).await.unwrap().is_empty());
    assert_eq!(*fake.bearers.lock().unwrap(), ["a1"]);
    assert_eq!(*fake.shown.lock().unwrap(), ["r1"]);
    assert_eq!(
        store.get(&url).unwrap().as_deref(),
        Some("r2"),
        "the rotated refresh token is kept"
    );
}

#[tokio::test]
async fn a_refused_access_token_is_replaced_and_the_call_sent_again_once() {
    let fake = Arc::new(Fake::default());
    fake.refreshes
        .lock()
        .unwrap()
        .extend([issued("a1", "r2"), issued("a2", "r3")]);
    fake.accepts.lock().unwrap().push("a2".into()); // the store restarted with a new key
    let (client, tokens, store, url, _dir) = rig(&fake).await;
    let token = current(&*tokens).await.unwrap().unwrap();
    assert!(client.list(1, Some(&token)).await.unwrap().is_empty());
    assert_eq!(*fake.bearers.lock().unwrap(), ["a1", "a2"]);
    assert_eq!(*fake.shown.lock().unwrap(), ["r1", "r2"]);
    assert_eq!(store.get(&url).unwrap().as_deref(), Some("r3"));
}

#[tokio::test]
async fn a_token_the_store_keeps_refusing_is_asked_for_once_more_and_then_the_refusal_stands() {
    let fake = Arc::new(Fake::default());
    fake.refreshes
        .lock()
        .unwrap()
        .extend([issued("a1", "r2"), issued("a2", "r3")]);
    let (client, tokens, _store, _url, _dir) = rig(&fake).await;
    let token = current(&*tokens).await.unwrap().unwrap();
    let err = client.list(1, Some(&token)).await.unwrap_err();
    assert!(
        matches!(err, crate::backlog::BacklogError::BadAnswer { status: 401, .. }),
        "{err:?}"
    );
    assert_eq!(fake.bearers.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_refused_refresh_token_is_no_link_and_nothing_is_sent_to_the_store() {
    let fake = Arc::new(Fake::default());
    fake.refreshes.lock().unwrap().push_back((
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_grant", "error_description": "Unknown refresh token" }),
    ));
    let (client, tokens, store, url, _dir) = rig(&fake).await;
    assert_eq!(current(&*tokens).await.unwrap(), None, "fails closed: not linked");
    assert_eq!(store.get(&url).unwrap(), None, "the refused refresh token is dropped");
    assert!(tokens.unlinked_on_site());
    assert!(!AccessTokens::linked(&*tokens));
    assert!(fake.bearers.lock().unwrap().is_empty());
    // A call made with no token is the store's own `unauthorized`, and no refresh is tried for it.
    let err = client.list(1, None).await.unwrap_err();
    assert!(
        matches!(err, crate::backlog::BacklogError::BadAnswer { status: 401, .. }),
        "{err:?}"
    );
    assert_eq!(fake.shown.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_refresh_refused_in_the_retry_leaves_the_401_and_unlinks() {
    let fake = Arc::new(Fake::default());
    fake.refreshes.lock().unwrap().extend([
        issued("a1", "r2"),
        (StatusCode::BAD_REQUEST, json!({ "error": "invalid_grant" })),
    ]);
    let (client, tokens, store, url, _dir) = rig(&fake).await;
    let token = current(&*tokens).await.unwrap().unwrap();
    let err = client.list(1, Some(&token)).await.unwrap_err();
    assert!(
        matches!(err, crate::backlog::BacklogError::BadAnswer { status: 401, .. }),
        "{err:?}"
    );
    assert_eq!(fake.bearers.lock().unwrap().len(), 1, "no second call without a token");
    assert_eq!(store.get(&url).unwrap(), None);
    assert!(crate::backlog::unlinked_on_site(), "no new link starts by itself");
    crate::backlog::clear_unlinked_on_site();
}

#[tokio::test]
async fn a_store_that_cant_be_reached_is_an_error_not_no_link() {
    let fake = Arc::new(Fake::default());
    let (_client, tokens, store, _url, dir) = rig(&fake).await;
    let down = Arc::new(DeviceTokens::new(
        "http://127.0.0.1:9".into(),
        BACKLOG_TARGET,
        store.clone(),
        reqwest::Client::new(),
    ));
    store.set("http://127.0.0.1:9", "r1").unwrap();
    assert!(current(&*down).await.is_err());
    drop((tokens, dir));
}
