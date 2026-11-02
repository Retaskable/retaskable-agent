use super::*;
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<Value>>>;

async fn store(status: u16, reply: Value) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                move |State((seen, reply)): State<(Seen, Value)>, Json(body): Json<Value>| async move {
                    seen.lock().unwrap().push(body);
                    (StatusCode::from_u16(status).unwrap(), Json(reply))
                },
            ),
        )
        .with_state((seen.clone(), reply));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn agent() -> ClaimNextAgent {
    ClaimNextAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: Vec::new(),
    }
}

#[tokio::test]
async fn draft_claim_next_sends_the_agent_and_reads_the_draft_and_its_token() {
    let (base, seen) = store(
        200,
        json!({ "value": { "token": "lease-9", "draft": {
            "id": "draft-3", "request": "Add charts", "titles": ["Charts"], "sessionId": "s-1",
            "intake": { "themes": ["Reports"], "trial": true, "parsed": true },
            "answer": { "answers": ["Yes"], "by": "ann" }, "rebaseReason": "main moved", "status": "queued" } } }),
    )
    .await;
    let claim = Client::new(base)
        .draft_claim_next(1, Some("tok"), &agent(), &["claude".to_string()])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.token, "lease-9");
    assert_eq!(claim.draft.id, "draft-3");
    assert_eq!(claim.draft.request.as_deref(), Some("Add charts"));
    assert_eq!(claim.draft.titles, ["Charts"]);
    assert_eq!(claim.draft.session_id.as_deref(), Some("s-1"));
    assert_eq!(claim.draft.rebase_reason.as_deref(), Some("main moved"));
    let intake = claim.draft.intake.unwrap();
    assert_eq!(intake.themes, ["Reports"]);
    assert!(intake.trial);
    assert_eq!(claim.draft.answer.unwrap().answers, ["Yes"]);
    let sent = &seen.lock().unwrap()[0];
    assert_eq!(sent["op"], "draft_claim_next");
    assert_eq!(sent["args"]["agent"]["machine"], "box");
    assert_eq!(sent["args"]["engines"], json!(["claude"]));
}

#[tokio::test]
async fn a_claim_with_nothing_waiting_is_none() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let claim = Client::new(base)
        .draft_claim_next(1, None, &agent(), &[])
        .await
        .unwrap();
    assert_eq!(claim, None);
    assert!(
        seen.lock().unwrap()[0]["args"].get("engines").is_none(),
        "left out when none"
    );
}

#[tokio::test]
async fn draft_report_sends_only_the_fields_given_with_the_lease() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let report = DraftReport {
        status: Some("needs_person".into()),
        summary: Some("Done".into()),
        questions: Some(vec!["Which?".into()]),
        pr_url: Some("https://github.com/x/y/pull/4".into()),
        item_ids: Some(vec!["a".into(), "b".into()]),
        breakdown: Some(json!({ "entries": [] })),
        ..Default::default()
    };
    Client::new(base)
        .draft_report(1, Some("tok"), "draft-3", "lease-9", &report)
        .await
        .unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "draft_report");
    let args = sent["args"].as_object().unwrap();
    assert_eq!(args["id"], "draft-3");
    assert_eq!(args["token"], "lease-9");
    assert_eq!(args["status"], "needs_person");
    assert_eq!(args["prUrl"], "https://github.com/x/y/pull/4");
    assert_eq!(args["itemIds"], json!(["a", "b"]));
    assert_eq!(args["questions"], json!(["Which?"]));
    assert!(!args.contains_key("titles"));
}

#[tokio::test]
async fn answer_taken_and_front_send_their_ops() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let client = Client::new(base);
    client.draft_answer_taken(1, None, "draft-3", "lease-9").await.unwrap();
    client.draft_front(1, None, "draft-4").await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "draft_answer_taken");
    assert_eq!(seen[0]["args"], json!({ "id": "draft-3", "token": "lease-9" }));
    assert_eq!(seen[1]["op"], "draft_front");
    assert_eq!(seen[1]["args"], json!({ "id": "draft-4" }));
}

#[tokio::test]
async fn upsert_from_specs_sends_the_headers_and_reads_the_added_ids() {
    let (base, seen) = store(200, json!({ "value": { "added": ["a"] } })).await;
    let items = [SpecItem {
        id: "a".into(),
        title: "First".into(),
        depends_on: vec![],
        spec_refs: vec!["docs/specs/x.md".into()],
        kind: Some("feature".into()),
        milestone: None,
        spec: "x.md".into(),
        line: 3,
    }];
    let asked = BTreeMap::from([("a".to_string(), "ann".to_string())]);
    let added = Client::new(base)
        .upsert_from_specs(
            1,
            None,
            &SpecsUpsert {
                commit: "abc",
                previous: Some("old"),
                items: &items,
                approved: &["a".to_string()],
                asked_by: &asked,
            },
        )
        .await
        .unwrap();
    assert_eq!(added, ["a"]);
    let args = seen.lock().unwrap()[0]["args"].clone();
    assert_eq!(args["commit"], "abc");
    assert_eq!(args["previous"], "old");
    assert_eq!(args["items"][0]["specRefs"], json!(["docs/specs/x.md"]));
    assert_eq!(args["approved"], json!(["a"]));
    assert_eq!(args["askedBy"], json!({ "a": "ann" }));
}

#[tokio::test]
async fn add_fix_sends_a_chat_todo_with_the_fix_prefix() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let mut fix = FixItem::new("fix-typo", "typo on the home page", " Correct it. ");
    fix.requested_by = Some("ann".into());
    Client::new(base).add_fix(1, None, &fix).await.unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "add");
    assert_eq!(sent["args"]["via"], "chat");
    let item = &sent["args"]["items"][0];
    assert_eq!(item["id"], "fix-typo");
    assert_eq!(item["title"], "Fix: typo on the home page");
    assert_eq!(item["kind"], "fix");
    assert_eq!(item["remaining"], "Correct it.");
    assert_eq!(item["requestedBy"], "ann");
    assert_eq!(FixItem::new("x", "Fix: already", "").title, "Fix: already");
}

#[tokio::test]
async fn a_store_error_is_a_backlog_error_for_every_op() {
    let (base, _) = store(409, json!({ "error": "conflict", "message": "not waiting" })).await;
    let c = Client::new(base);
    let e = c.draft_front(1, None, "d").await.unwrap_err();
    assert_eq!(e.code(), Some("conflict"));
    assert!(c.draft_claim_next(1, None, &agent(), &[]).await.is_err());
    assert!(
        c.draft_report(1, None, "d", "t", &DraftReport::default())
            .await
            .is_err()
    );
    assert!(c.draft_answer_taken(1, None, "d", "t").await.is_err());
    assert!(c.add_fix(1, None, &FixItem::new("a", "b", "")).await.is_err());
}

#[tokio::test]
async fn an_unreachable_store_is_unreachable_never_none() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let err = Client::new(format!("http://{addr}"))
        .draft_claim_next(1, None, &agent(), &[])
        .await
        .unwrap_err();
    assert!(matches!(err, BacklogError::Unreachable(_)), "{err:?}");
}
