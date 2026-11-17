use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use serde_json::{Map, Value, json};

use super::*;

/// The store's side of one item's details: version, fields, how many reports to refuse as `conflict` (another writer
/// got in first: its `ghFailures` bump lands before each refusal) and how many to fail outright.
#[derive(Default)]
struct Item {
    version: u64,
    details: Map<String, Value>,
    sneak: u32,
    down: u32,
    reports: Vec<Value>,
}

type Shared = Arc<Mutex<Item>>;

async fn handle(State(item): State<Shared>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut item = item.lock().unwrap();
    let args = &body["args"];
    let record = |i: &Item| json!({ "version": i.version, "details": i.details });
    match body["op"].as_str() {
        Some("details_get") if item.version == 0 => (StatusCode::OK, Json(json!({ "value": null }))),
        Some("details_get") => (StatusCode::OK, Json(json!({ "value": record(&item) }))),
        Some("details_report") => {
            item.reports.push(args.clone());
            if item.down > 0 {
                item.down -= 1;
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "down" })));
            }
            if item.sneak > 0 {
                item.sneak -= 1;
                item.details.insert("ghFailures".into(), json!(9));
                item.version += 1;
            }
            if args["baseVersion"].as_u64() != Some(item.version) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({ "error": "conflict", "message": "moved" })),
                );
            }
            if let Some(patch) = args["patch"].as_object() {
                for (k, v) in patch {
                    if v.is_null() {
                        item.details.remove(k);
                    } else {
                        item.details.insert(k.clone(), v.clone());
                    }
                }
            }
            item.version += 1;
            (StatusCode::OK, Json(json!({ "value": record(&item) })))
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad_request" }))),
    }
}

async fn fake(item: Item) -> (Client, Shared) {
    let item: Shared = Arc::new(Mutex::new(item));
    let app = Router::new()
        .route("/v1/repos/{id}/ops", post(handle))
        .with_state(item.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (Client::new(format!("http://{addr}")), item)
}

async fn loaded(c: &Client) -> ItemDecisions {
    ItemDecisions::load(c, 1, Some("t"), "a").await.unwrap()
}

#[tokio::test]
async fn a_fresh_item_loads_empty_and_a_flush_with_nothing_changed_sends_nothing() {
    let (c, item) = fake(Item::default()).await;
    let mut d = loaded(&c).await;
    assert_eq!(d.now, Decisions::default());
    assert!(!d.dirty());
    d.flush(&c, 1, Some("t"), Some("L"), "a").await.unwrap();
    assert!(item.lock().unwrap().reports.is_empty());
}

#[tokio::test]
async fn a_flush_sends_only_what_changed_with_the_lease_and_takes_the_stores_record() {
    let (c, item) = fake(Item {
        version: 2,
        details: json!({ "attempt": 1, "reasks": 4 }).as_object().unwrap().clone(),
        ..Default::default()
    })
    .await;
    let mut d = loaded(&c).await;
    assert_eq!(d.now.attempt, Some(1));
    d.now.attempt = Some(2);
    assert!(d.dirty());
    d.flush(&c, 1, Some("t"), Some("L"), "a").await.unwrap();
    assert!(!d.dirty());
    let store = item.lock().unwrap();
    assert_eq!(store.reports.len(), 1);
    assert_eq!(store.reports[0]["patch"], json!({ "attempt": 2 }));
    assert_eq!(store.reports[0]["baseVersion"], 2);
    assert_eq!(store.reports[0]["token"], "L");
    assert_eq!(store.version, 3);
}

#[tokio::test]
async fn details_that_moved_are_read_again_and_the_patch_sent_on_top() {
    let (c, item) = fake(Item {
        version: 1,
        sneak: 1,
        ..Default::default()
    })
    .await;
    let mut d = loaded(&c).await;
    d.now.attempt = Some(3);
    d.flush(&c, 1, None, None, "a").await.unwrap();
    // Our field and the other writer's both survive.
    assert_eq!(d.now.attempt, Some(3));
    assert_eq!(d.now.gh_failures, Some(9));
    assert!(!d.dirty());
    let store = item.lock().unwrap();
    assert_eq!(store.reports.len(), 2);
    assert_eq!(store.details["attempt"], 3);
    assert_eq!(store.details["ghFailures"], 9);
}

#[tokio::test]
async fn a_store_that_keeps_moving_gives_up_after_three_resends_and_keeps_the_edit() {
    let (c, item) = fake(Item {
        version: 1,
        sneak: 99,
        ..Default::default()
    })
    .await;
    let mut d = loaded(&c).await;
    d.now.attempt = Some(3);
    let e = d.flush(&c, 1, None, None, "a").await.unwrap_err();
    assert_eq!(e.code(), Some("conflict"));
    assert_eq!(item.lock().unwrap().reports.len(), 4);
    assert_eq!(d.now.attempt, Some(3));
    assert!(d.dirty());
}

#[tokio::test]
async fn a_store_that_is_down_leaves_the_edit_for_the_next_flush() {
    let (c, item) = fake(Item {
        version: 1,
        down: 1,
        ..Default::default()
    })
    .await;
    let mut d = loaded(&c).await;
    d.now.failed_in = Some("ci".into());
    assert!(d.flush(&c, 1, None, None, "a").await.is_err());
    assert!(d.dirty());
    d.flush(&c, 1, None, None, "a").await.unwrap();
    assert!(!d.dirty());
    assert_eq!(item.lock().unwrap().details["failedIn"], "ci");
}

#[tokio::test]
async fn a_cleared_field_is_sent_as_null() {
    let (c, item) = fake(Item {
        version: 1,
        details: json!({ "failedIn": "ci" }).as_object().unwrap().clone(),
        ..Default::default()
    })
    .await;
    let mut d = loaded(&c).await;
    d.now.failed_in = None;
    d.flush(&c, 1, None, None, "a").await.unwrap();
    let store = item.lock().unwrap();
    assert_eq!(store.reports[0]["patch"], json!({ "failedIn": null }));
    assert!(store.details.get("failedIn").is_none());
}

#[test]
fn a_pushed_record_updates_what_the_store_changed_and_keeps_unsent_edits() {
    let mut d = ItemDecisions::default();
    d.now.attempt = Some(2);
    let pushed = DetailsRecord {
        version: 5,
        details: json!({ "reasks": 7, "attempt": 1 }),
    };
    d.refresh(&pushed);
    assert_eq!(d.now.reasks, Some(7));
    assert_eq!(d.now.attempt, Some(2));
    assert!(d.dirty());
    // An older record changes nothing.
    d.refresh(&DetailsRecord {
        version: 4,
        details: json!({ "reasks": 0 }),
    });
    assert_eq!(d.now.reasks, Some(7));
}
