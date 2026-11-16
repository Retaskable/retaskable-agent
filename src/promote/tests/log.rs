//! The deploy log's queue against a fake store: a row is on disk before it is sent, kept while the store is down,
//! sent in order when it isn't, and gone from disk only once the store has answered it.
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::backlog::Client;
use crate::promote::DeployLog;
use crate::promote::log::{FILE, RETRY_MS};

fn row(to: &str) -> Map<String, Value> {
    json!({ "at": "2023-11-14T22:13:20.000Z", "login": "ann", "environment": "production",
            "from": "a", "to": to, "outcome": "pushed" })
    .as_object()
    .unwrap()
    .clone()
}

/// What the store was sent, and whether the row was already in the file when it arrived.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(Value, bool)>>>);

/// A store answering every op `status` with `error` (when not 200), looking at `file` as each op arrives.
async fn store(file: &Path, status: u16, error: Option<&str>) -> (String, Seen) {
    let seen = Seen::default();
    let state = (seen.clone(), file.to_path_buf(), status, error.map(String::from));
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                |State((seen, file, status, error)): State<(Seen, std::path::PathBuf, u16, Option<String>)>,
                 Json(body): Json<Value>| async move {
                    let on_disk = std::fs::read_to_string(&file).unwrap_or_default();
                    let held = on_disk.contains(body["args"]["entry"]["to"].as_str().unwrap_or("?"));
                    seen.0.lock().unwrap().push((body, held));
                    let reply = error.map_or(json!({ "value": null }), |e| json!({ "error": e }));
                    (StatusCode::from_u16(status).unwrap(), Json(reply))
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn tos(seen: &Seen) -> Vec<String> {
    seen.0
        .lock()
        .unwrap()
        .iter()
        .map(|(b, _)| b["args"]["entry"]["to"].as_str().unwrap().into())
        .collect()
}

#[tokio::test]
async fn a_row_is_on_disk_before_anything_is_sent_and_gone_once_the_store_took_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    let (base, seen) = store(&file, 200, None).await;
    let log = DeployLog::open(&file);
    log.add("deploy", row("c1"));
    // Nothing sent yet, and the file has it.
    assert!(seen.0.lock().unwrap().is_empty());
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(on_disk[0]["kind"], "deploy");
    assert_eq!(on_disk[0]["entry"], json!(row("c1")));
    log.flush(&Client::new(&base), 5, "tok", 0).await;
    {
        let sent = seen.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0["op"], "log_add");
        assert_eq!(sent[0].0["args"], json!({ "kind": "deploy", "entry": row("c1") }));
        assert!(sent[0].1, "the row was in the file when it was sent");
    }
    assert!(log.pending().is_empty());
    assert!(!file.exists(), "pruned once the store has it");
}

#[tokio::test]
async fn a_store_that_is_down_keeps_every_row_and_the_next_pass_sends_them_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    let log = DeployLog::open(&file);
    log.add("deploy", row("c1"));
    log.add("deploy", row("c2"));
    // Nothing is listening.
    log.flush_due(&Client::new("http://127.0.0.1:1"), 5, "tok", 1_000).await;
    assert_eq!(log.pending().len(), 2);
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept.as_array().unwrap().len(), 2);

    let (base, seen) = store(&file, 200, None).await;
    let client = Client::new(&base);
    // Too soon: it waits its 30 seconds.
    log.flush_due(&client, 5, "tok", 1_000 + RETRY_MS - 1).await;
    assert!(seen.0.lock().unwrap().is_empty());
    log.flush_due(&client, 5, "tok", 1_000 + RETRY_MS).await;
    assert_eq!(tos(&seen), ["c1", "c2"], "oldest first");
    assert!(log.pending().is_empty());
    assert!(!file.exists());
}

#[tokio::test]
async fn a_row_the_store_did_not_take_stays_and_stops_the_ones_behind_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    let (base, seen) = store(&file, 500, Some("internal")).await;
    let log = DeployLog::open(&file);
    log.add("deploy", row("c1"));
    log.add("deploy", row("c2"));
    log.flush(&Client::new(&base), 5, "tok", 0).await;
    assert_eq!(tos(&seen), ["c1"], "in order: c2 waits for c1");
    assert_eq!(log.pending().len(), 2);
    assert!(file.exists());
}

#[tokio::test]
async fn a_restart_reads_back_what_was_never_sent() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    DeployLog::open(&file).add("deploy", row("c1"));
    let again = DeployLog::open(&file);
    assert_eq!(again.pending().len(), 1);
    again.add("deploy", row("c2"));
    let (base, seen) = store(&file, 200, None).await;
    again.flush(&Client::new(&base), 5, "tok", 0).await;
    assert_eq!(tos(&seen), ["c1", "c2"]);
}

#[tokio::test]
async fn a_row_of_another_kind_a_file_holds_is_kept_and_sent_as_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    std::fs::write(
        &file,
        json!([{ "kind": "rollout", "at": 1, "entry": row("r1") }]).to_string(),
    )
    .unwrap();
    let log = DeployLog::open(&file);
    log.add("deploy", row("c1"));
    let (base, seen) = store(&file, 200, None).await;
    log.flush(&Client::new(&base), 5, "tok", 0).await;
    let kinds: Vec<_> = seen
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|(b, _)| b["args"]["kind"].clone())
        .collect();
    assert_eq!(kinds, ["rollout", "deploy"]);
}

#[tokio::test]
async fn only_a_row_the_store_calls_a_bad_request_is_dropped_and_it_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    let (base, seen) = store(&file, 400, Some("bad_request")).await;
    let log = DeployLog::open(&file);
    log.add("deploy", row("c1"));
    log.add("deploy", row("c2"));
    log.flush(&Client::new(&base), 5, "tok", 0).await;
    // Each was answered (refused) and so left the queue; neither was lost without the store having been asked.
    assert_eq!(tos(&seen), ["c1", "c2"]);
    assert!(log.pending().is_empty());
}

#[tokio::test]
async fn a_file_that_is_not_a_queue_is_moved_aside_not_written_over() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    std::fs::write(&file, "{ not a queue").unwrap();
    let log = DeployLog::open(&file);
    assert!(log.pending().is_empty());
    log.add("deploy", row("c1"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(format!("{FILE}.bad"))).unwrap(),
        "{ not a queue"
    );
    assert_eq!(log.pending().len(), 1);
}

#[tokio::test]
async fn every_holder_of_one_file_shares_one_queue() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(FILE);
    let (a, b) = (DeployLog::shared(&file), DeployLog::shared(&file));
    assert!(Arc::ptr_eq(&a, &b));
    a.add("deploy", row("c1"));
    assert_eq!(b.pending().len(), 1);
}
