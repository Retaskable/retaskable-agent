use super::*;
use crate::backlog::tests::store;

#[tokio::test]
async fn list_reads_the_rows_and_ignores_what_it_does_not_model() {
    let rows = json!({ "value": [
        { "id": "dr-1", "kind": "testflight_build", "args": { "sha": "abc1234", "environment": "production", "prefer": "a-pc" },
          "issuedBy": "Ann", "at": 5, "status": "requested" },
        { "id": "dr-2", "kind": "go_live_prepare", "args": { "provider": "cloudflare" }, "issuedBy": "bo", "at": 6,
          "status": "running", "claim": { "agentId": "9", "machine": "m", "at": 1, "beatAt": 2 }, "result": { "x": 1 } },
        { "id": "dr-3", "kind": "hosting", "args": {}, "issuedBy": "bo", "at": 7, "status": "done", "finishedAt": 9 },
    ] });
    let (base, seen) = store(200, rows).await;
    let got = Client::new(base).deploy_list(1, Some("t")).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0]["op"], "deploy_list");
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].kind, DeployKind::TestflightBuild);
    assert_eq!(got[0].status, DeployStatus::Requested);
    assert_eq!(got[0].args.prefer.as_deref(), Some("a-pc"));
    assert_eq!(got[1].args.provider.as_deref(), Some("cloudflare"));
    assert_eq!(got[1].status, DeployStatus::Running);
    assert_eq!(got[2].status, DeployStatus::Done);
}

#[tokio::test]
async fn a_kind_the_store_adds_later_is_a_bad_answer_not_a_guess() {
    let rows = json!({ "value": [{ "id": "x", "kind": "new_thing", "args": {}, "issuedBy": "a", "at": 1, "status": "requested" }] });
    let (base, _) = store(200, rows).await;
    let err = Client::new(base).deploy_list(1, None).await.unwrap_err();
    assert!(matches!(err, BacklogError::BadAnswer { .. }), "{err:?}");
}

#[tokio::test]
async fn claim_always_says_it_can_and_reads_who_won() {
    let (base, seen) = store(200, json!({ "value": { "won": true, "staleMs": 600000 } })).await;
    assert!(Client::new(base).deploy_claim(1, None, "dr-1", "a-pc").await.unwrap());
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "deploy_claim");
    assert_eq!(sent["args"], json!({ "id": "dr-1", "machine": "a-pc", "can": true }));

    let (base, _) = store(
        200,
        json!({ "value": { "won": false, "status": "claimed", "holder": "b-pc" } }),
    )
    .await;
    assert!(!Client::new(base).deploy_claim(1, None, "dr-1", "a-pc").await.unwrap());
}

#[tokio::test]
async fn beat_and_finish_read_whether_the_claim_is_still_held() {
    let (base, seen) = store(200, json!({ "value": { "ok": false } })).await;
    let client = Client::new(base);
    assert!(!client.deploy_beat(1, None, "dr-1").await.unwrap());
    let mut result = Map::new();
    result.insert("why".into(), json!("Name a commit."));
    assert!(
        !client
            .deploy_finish(1, None, "dr-1", Finish::Refused, Some(result))
            .await
            .unwrap()
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["args"], json!({ "id": "dr-1" }));
    assert_eq!(
        seen[1]["args"],
        json!({ "id": "dr-1", "status": "refused", "result": { "why": "Name a commit." } })
    );
}

#[tokio::test]
async fn finish_without_a_result_sends_none() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    assert!(
        Client::new(base)
            .deploy_finish(1, None, "dr-1", Finish::Done, None)
            .await
            .unwrap()
    );
    assert_eq!(
        seen.lock().unwrap()[0]["args"],
        json!({ "id": "dr-1", "status": "done" })
    );
}

#[tokio::test]
async fn a_refused_op_keeps_the_stores_code() {
    let (base, _) = store(
        400,
        json!({ "error": "bad_request", "message": "result must not hold a secret" }),
    )
    .await;
    let err = Client::new(base)
        .deploy_finish(1, None, "dr-1", Finish::Done, None)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("bad_request"));
}

#[test]
fn a_result_over_four_kb_is_not_sent() {
    let mut big = Map::new();
    big.insert("out".into(), json!("x".repeat(RESULT_MAX_BYTES)));
    assert!(checked_result(big).unwrap_err().contains("4 KB"));
    let mut small = Map::new();
    small.insert("sha".into(), json!("abc1234"));
    assert!(checked_result(small).is_ok());
}

#[test]
fn every_kind_has_the_stores_name() {
    let names: Vec<_> = DeployKind::ALL.iter().map(|k| k.name()).collect();
    assert_eq!(names, ["testflight_build", "promote", "hosting", "go_live_prepare"]);
    for k in DeployKind::ALL {
        let parsed: DeployKind = serde_json::from_value(json!(k.name())).unwrap();
        assert_eq!(parsed, k);
    }
}
