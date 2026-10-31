use super::*;
use axum::{Json, Router, routing::post};
use std::net::TcpListener as StdTcpListener;

#[test]
fn url_defaults_then_takes_the_env_and_trims_a_trailing_slash() {
    assert_eq!(url(&Env::empty()), DEFAULT_URL);
    let env = Env::from_pairs([("RETASKABLE_BACKLOG_URL", "https://example.test/")]);
    assert_eq!(url(&env), "https://example.test");
    let blank = Env::from_pairs([("RETASKABLE_BACKLOG_URL", "   ")]);
    assert_eq!(url(&blank), DEFAULT_URL);
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

fn test_agent() -> ClaimNextAgent {
    ClaimNextAgent {
        machine: "box-1".to_string(),
        environment: "Production".to_string(),
        base_branch: "main".to_string(),
        tags: Vec::new(),
    }
}

/// A `reopen` request with no `by`, `what` or `steps`, for tests that don't care about them.
fn reopen_request() -> ReopenRequest<'static> {
    ReopenRequest {
        item_id: "a",
        notes: "The chart didn't load.",
        by: None,
        what: None,
        steps: None,
    }
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
        assert!(
            req["args"]["agent"].as_object().unwrap().get("tags").is_none(),
            "left out when this computer has none"
        );
        Json(json!({
            "value": {
                "item": { "id": "a", "title": "First", "status": "claimed", "rank": 0 },
                "token": "lease-1",
            }
        }))
    }))
    .await;

    let agent = test_agent();
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
async fn claim_next_sends_this_computers_tags_when_it_has_any() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        assert_eq!(req["args"]["agent"]["tags"], json!(["windows", "gpu"]));
        Json(json!({ "value": null }))
    }))
    .await;

    let agent = ClaimNextAgent {
        tags: vec!["windows".to_string(), "gpu".to_string()],
        ..test_agent()
    };
    Client::new(base).claim_next(1, Some("tok"), &agent).await.unwrap();
}

#[tokio::test]
async fn claim_next_reads_a_null_value_as_nothing_not_a_bad_answer() {
    let base = fake_store(post(|| async { Json(json!({ "value": null })) })).await;

    let agent = test_agent();
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
async fn tryouts_list_sends_no_args_and_reads_the_waiting_queue_and_its_last_grouping() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        assert_eq!(req["op"], "tryouts_list");
        assert_eq!(req["args"], json!({}));
        Json(json!({
            "value": {
                "waiting": [
                    { "key": "a@2026-10-28T00:00:00Z", "itemId": "a", "title": "Charts",
                      "what": "Open the charts page", "steps": ["Open it"] },
                    { "key": "b@main@sha1", "itemId": "b", "title": "Second", "what": "Look at it",
                      "steps": [], "onBranch": { "branch": "main", "sha": "sha1" } },
                ],
                "groups": { "signature": "a@2026-10-28T00:00:00Z", "at": 1000 },
            }
        }))
    }))
    .await;

    let list = Client::new(base).tryouts_list(1, Some("tok")).await.unwrap();
    assert_eq!(list.waiting.len(), 2);
    assert_eq!(list.waiting[0].steps, vec!["Open it".to_string()]);
    assert!(list.waiting[0].on_branch.is_none());
    assert_eq!(
        list.waiting[1].on_branch,
        Some(OnBranch {
            branch: "main".to_string(),
            sha: "sha1".to_string(),
        })
    );
    let groups = list.groups.unwrap();
    assert_eq!(groups.signature, "a@2026-10-28T00:00:00Z");
    assert_eq!(groups.at, 1000);
}

#[tokio::test]
async fn tryouts_list_reads_no_groups_as_none() {
    let base = fake_store(post(|| async { Json(json!({ "value": { "waiting": [] } })) })).await;

    let list = Client::new(base).tryouts_list(1, Some("tok")).await.unwrap();
    assert!(list.waiting.is_empty());
    assert!(list.groups.is_none());
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
async fn release_sends_the_item_id_and_its_lease_token() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        assert_eq!(req["op"], "release");
        assert_eq!(req["args"], json!({ "id": "a", "token": "lease-1" }));
        Json(json!({}))
    }))
    .await;

    Client::new(base).release(1, Some("tok"), "a", "lease-1").await.unwrap();
}

#[tokio::test]
async fn report_sends_the_item_id_its_lease_token_and_the_note() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        assert_eq!(req["op"], "report");
        assert_eq!(
            req["args"],
            json!({ "id": "a", "token": "lease-1", "note": "a computer's own failure" })
        );
        Json(json!({}))
    }))
    .await;

    Client::new(base)
        .report(1, Some("tok"), "a", "lease-1", "a computer's own failure")
        .await
        .unwrap();
}

#[tokio::test]
async fn park_sends_the_item_id_its_lease_token_and_what_it_waits_for() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        assert_eq!(req["op"], "park");
        assert_eq!(req["args"], json!({ "id": "a", "token": "lease-1", "for": "answer" }));
        Json(json!({}))
    }))
    .await;

    Client::new(base)
        .park(1, Some("tok"), "a", "lease-1", "answer")
        .await
        .unwrap();
}

#[tokio::test]
async fn release_keeps_the_stores_refusal_code() {
    let base = fake_store(post(|| async {
        (
            axum::http::StatusCode::CONFLICT,
            Json(json!({ "error": "stale_lease", "message": "gone" })),
        )
    }))
    .await;

    let err = Client::new(base).release(1, Some("tok"), "a", "old").await.unwrap_err();
    assert_eq!(err.code(), Some("stale_lease"));
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
async fn reopen_sends_the_notes_and_agent_then_reads_back_the_claimed_item() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        assert_eq!(req["op"], "reopen");
        assert_eq!(req["args"]["id"], "a");
        assert_eq!(req["args"]["notes"], "The chart didn't load.");
        assert_eq!(req["args"]["by"], "jcn");
        assert_eq!(req["args"]["what"], "Open the charts page");
        assert_eq!(req["args"]["steps"][0], "Open it");
        assert_eq!(req["args"]["agent"]["machine"], "box-1");
        Json(json!({
            "value": {
                "reopened": true,
                "item": { "id": "a", "title": "First", "status": "todo", "rank": 0 },
                "token": "lease-1",
            }
        }))
    }))
    .await;

    let agent = test_agent();
    let req = ReopenRequest {
        by: Some("jcn"),
        what: Some("Open the charts page"),
        steps: Some(&["Open it".to_string()]),
        ..reopen_request()
    };
    let answer = Client::new(base).reopen(1, Some("tok"), &req, &agent).await.unwrap();
    match answer {
        ReopenAnswer::Reopened { item, token } => {
            assert_eq!(item.id, "a");
            assert_eq!(token, "lease-1");
        }
        ReopenAnswer::Kept => panic!("expected it reopened"),
    }
}

#[tokio::test]
async fn reopen_leaves_out_by_what_and_steps_when_there_are_none() {
    let base = fake_store(post(|Json(req): Json<Value>| async move {
        let args = req["args"].as_object().unwrap();
        assert!(args.get("by").is_none());
        assert!(args.get("what").is_none());
        assert!(args.get("steps").is_none());
        Json(json!({ "value": { "reopened": false } }))
    }))
    .await;

    let agent = test_agent();
    let req = reopen_request();
    Client::new(base).reopen(1, None, &req, &agent).await.unwrap();
}

#[tokio::test]
async fn reopen_is_kept_when_the_store_declines_it() {
    let base = fake_store(post(|| async { Json(json!({ "value": { "reopened": false } })) })).await;

    let agent = test_agent();
    let req = reopen_request();
    let answer = Client::new(base).reopen(1, Some("tok"), &req, &agent).await.unwrap();
    assert_eq!(answer, ReopenAnswer::Kept);
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
    let env = Env::from_pairs([("RETASKABLE_BACKLOG_URL", base.clone())]);
    let (_dir, store) = file_store();
    let mut shown = None;
    let id = auto_link(store.clone(), reqwest::Client::new(), &env, "9.9.9", |c| {
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
    let env = Env::from_pairs([("RETASKABLE_BACKLOG_URL", base.clone())]);
    let (_dir, store) = file_store();
    store.set(&base, "already-linked").unwrap();
    let id = auto_link(store, reqwest::Client::new(), &env, "9.9.9", |_| {
        panic!("no code expected")
    })
    .await
    .unwrap();
    assert_eq!(id, None);
}

#[tokio::test]
async fn auto_link_does_nothing_for_a_bad_backlog_url() {
    let env = Env::from_pairs([("RETASKABLE_BACKLOG_URL", "not a url")]);
    let (_dir, store) = file_store();
    let id = auto_link(store, reqwest::Client::new(), &env, "9.9.9", |_| {
        panic!("no code expected")
    })
    .await
    .unwrap();
    assert_eq!(id, None);
}
