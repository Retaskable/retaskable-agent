use super::*;
use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use std::sync::{Arc, Mutex};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// What the store keeps of one item for `complete` and `details_report`: the last completion by lease, the slice
/// count and the details version. It answers as shared/backlogComplete.ts does.
#[derive(Default)]
struct Item {
    lease: String,
    slice: u32,
    completed: Option<(String, Option<String>, Completed)>,
    version: u64,
    details: Map<String, Value>,
}

type Shared = Arc<Mutex<Item>>;

fn refuse(code: &str) -> (StatusCode, Json<Value>) {
    (StatusCode::CONFLICT, Json(json!({ "error": code, "message": code })))
}

async fn handle(State(item): State<Shared>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut item = item.lock().unwrap();
    let args = &body["args"];
    let token = args["token"].as_str().unwrap_or_default().to_string();
    let sha = args["mergeCommit"].as_str().map(String::from);
    match body["op"].as_str() {
        Some("complete") => {
            if let Some((lease, first, answer)) = &item.completed
                && *lease == token
            {
                if first.is_some() && sha.is_some() && *first != sha {
                    return refuse("conflict");
                }
                return (StatusCode::OK, Json(json!({ "value": answer })));
            }
            if token != item.lease {
                return refuse("stale_lease");
            }
            if args["remaining"].is_string() {
                item.slice += 1;
            }
            if !args["playtest"].is_null() {
                item.details.insert("implPlaytest".into(), args["playtest"].clone());
                item.version += 1;
            }
            let built = args["built"].as_bool().unwrap_or(false);
            let status = if built { "on_branch" } else { "todo" };
            let answer = Completed {
                built,
                status: status.into(),
            };
            item.completed = Some((token, sha, answer.clone()));
            (StatusCode::OK, Json(json!({ "value": answer })))
        }
        Some("details_report") => {
            if args["baseVersion"].as_u64() != Some(item.version) {
                return refuse("conflict");
            }
            if let Some(patch) = args["patch"].as_object() {
                item.details.extend(patch.clone());
            }
            item.version += 1;
            let record = json!({ "version": item.version, "details": item.details });
            (StatusCode::OK, Json(json!({ "value": record })))
        }
        _ => refuse("bad_request"),
    }
}

async fn fake(lease: &str) -> (Client, Shared) {
    let item: Shared = Arc::new(Mutex::new(Item {
        lease: lease.into(),
        ..Default::default()
    }));
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

fn merged(sha: &str) -> Completion {
    Completion {
        built: true,
        merge_commit: Some(sha.into()),
        remaining: Some(String::new()),
        ..Default::default()
    }
}

#[tokio::test]
async fn complete_sends_one_call_with_everything_and_leaves_out_what_is_unset() {
    let reply = json!({ "value": { "built": true, "status": "on_main" } });
    let (base, seen) = crate::backlog::tests::store(200, reply).await;
    let c = Client::new(base);
    let full = Completion {
        built: true,
        merge_commit: Some(SHA.into()),
        on_default_branch: true,
        remaining: Some("the edge cases".into()),
        playtest: Some(Playtest {
            what: "the chart".into(),
            steps: vec!["open it".into()],
        }),
    };
    let answer = c.complete(7, Some("tok"), "a", "L1", &full).await.unwrap();
    assert_eq!(
        answer,
        Completed {
            built: true,
            status: "on_main".into()
        }
    );
    c.complete(7, Some("tok"), "a", "L1", &Completion::default())
        .await
        .unwrap();
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent[0]["op"], "complete");
    assert_eq!(
        sent[0]["args"],
        json!({ "id": "a", "token": "L1", "built": true, "mergeCommit": SHA, "onDefaultBranch": true,
            "remaining": "the edge cases", "playtest": { "what": "the chart", "steps": ["open it"] } })
    );
    assert_eq!(sent[1]["args"], json!({ "id": "a", "token": "L1", "built": false }));
}

#[tokio::test]
async fn the_same_lease_completing_twice_counts_the_slice_once_and_answers_the_same() {
    let (c, item) = fake("L1").await;
    let first = c.complete(1, None, "a", "L1", &merged(SHA)).await.unwrap();
    let again = c.complete(1, None, "a", "L1", &merged(SHA)).await.unwrap();
    assert_eq!(first, again);
    assert!(first.built);
    assert_eq!(item.lock().unwrap().slice, 1);
}

#[tokio::test]
async fn another_token_is_stale_and_the_same_token_with_another_commit_conflicts() {
    let (c, _) = fake("L1").await;
    let stale = c.complete(1, None, "a", "L0", &merged(SHA)).await.unwrap_err();
    assert_eq!(stale.code(), Some("stale_lease"));
    c.complete(1, None, "a", "L1", &merged(SHA)).await.unwrap();
    let other = "f".repeat(40);
    let conflict = c.complete(1, None, "a", "L1", &merged(&other)).await.unwrap_err();
    assert_eq!(conflict.code(), Some("conflict"));
}

#[tokio::test]
async fn the_playtest_goes_up_with_the_completion_and_the_repeat_does_not_write_it_again() {
    let (c, item) = fake("L1").await;
    let mut done = merged(SHA);
    done.playtest = Some(Playtest {
        what: "w".into(),
        steps: vec!["s".into()],
    });
    c.complete(1, None, "a", "L1", &done).await.unwrap();
    c.complete(1, None, "a", "L1", &done).await.unwrap();
    let item = item.lock().unwrap();
    assert_eq!(item.details["implPlaytest"], json!({ "what": "w", "steps": ["s"] }));
    assert_eq!(item.version, 1);
}

#[tokio::test]
async fn a_not_built_completion_without_remaining_counts_no_slice() {
    let (c, item) = fake("L1").await;
    let free = Completion {
        merge_commit: Some(SHA.into()),
        ..Default::default()
    };
    let answer = c.complete(1, None, "a", "L1", &free).await.unwrap();
    assert!(!answer.built);
    assert_eq!(item.lock().unwrap().slice, 0);
}

#[tokio::test]
async fn details_report_sends_only_the_patch_and_reads_the_record_back() {
    let (c, item) = fake("L1").await;
    let pt = Playtest {
        what: "w".into(),
        steps: vec![],
    };
    let first = c.report_impl_playtest(1, None, "a", "L1", 0, &pt).await.unwrap();
    assert_eq!(first.version, 1);
    assert_eq!(first.details["implPlaytest"]["what"], "w");
    // A report naming a version the store has moved past is refused as `conflict`.
    let stale = c.report_impl_playtest(1, None, "a", "L1", 0, &pt).await.unwrap_err();
    assert_eq!(stale.code(), Some("conflict"));
    let mut patch = Map::new();
    patch.insert("pendingPlaytest".into(), json!({ "what": "later", "steps": [] }));
    let report = DetailsReport {
        id: "a",
        lease: None,
        base_version: 1,
        patch,
        note: Some("held for the playtest"),
    };
    let second = c.details_report(1, None, &report).await.unwrap();
    assert_eq!(second.version, 2);
    assert_eq!(item.lock().unwrap().details.len(), 2);
}

#[tokio::test]
async fn details_report_leaves_out_an_unset_token_and_note() {
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": { "version": 3 } })).await;
    let c = Client::new(base);
    let report = DetailsReport {
        id: "a",
        lease: None,
        base_version: 2,
        patch: Map::new(),
        note: None,
    };
    let record = c.details_report(1, None, &report).await.unwrap();
    assert_eq!(record.version, 3);
    let args = seen.lock().unwrap()[0]["args"].clone();
    assert_eq!(args, json!({ "id": "a", "baseVersion": 2, "patch": {} }));
}
