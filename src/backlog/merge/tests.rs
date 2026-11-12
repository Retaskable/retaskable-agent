use super::*;
use crate::backlog::tests::store;

#[tokio::test]
async fn enqueue_sends_the_change_and_marks_a_chore() {
    let (base, seen) = store(200, json!({ "value": { "entry": "e1", "place": 1 } })).await;
    let e = MergeEntry {
        item: "chore-3",
        base: "main",
        head: "abc",
        pr: "https://x/pull/4",
        land: "retaskable",
        chore: true,
        ..Default::default()
    };
    let v = Client::new(base).merge_enqueue(1, Some("t"), &e).await.unwrap();
    assert_eq!(v["place"], 1);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "merge_enqueue");
    assert_eq!(sent["args"]["chore"], true);
    assert_eq!(sent["args"]["head"], "abc");
    assert!(sent["args"].get("piece").is_none());
}

#[tokio::test]
async fn next_reads_a_turn_a_place_and_nothing() {
    let turn = json!({ "value": { "turn": { "entry": "e", "token": "T1", "leaseMs": 300000,
        "piece": { "id": "p1", "title": "P", "branch": "b", "change": "https://x/pull/4" } } } });
    let (base, seen) = store(200, turn).await;
    let got = Client::new(base).merge_next(1, None, "main", true).await.unwrap();
    let MergeNext::Turn(t) = got else { panic!("{got:?}") };
    assert_eq!((t.token.as_str(), t.lease_ms), ("T1", 300000));
    assert_eq!(t.piece.unwrap().branch.as_deref(), Some("b"));
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["args"], json!({ "base": "main", "piece": true }));

    let (base, _) = store(200, json!({ "value": { "waiting": { "place": 2, "ahead": 1 } } })).await;
    let got = Client::new(base).merge_next(1, None, "main", false).await.unwrap();
    assert_eq!(got, MergeNext::Waiting { place: 2, ahead: 1 });

    let (base, seen) = store(200, json!({ "value": null })).await;
    let got = Client::new(base).merge_next(1, None, "main", false).await.unwrap();
    assert_eq!(got, MergeNext::Nothing);
    assert!(seen.lock().unwrap()[0]["args"].get("piece").is_none());
}

#[tokio::test]
async fn a_store_that_cant_be_reached_is_an_error_never_a_turn() {
    let (base, _) = store(500, json!({ "error": "boom" })).await;
    assert!(Client::new(base).merge_next(1, None, "main", true).await.is_err());
}

#[tokio::test]
async fn beat_open_withdraw_and_result_send_their_arguments() {
    let (base, seen) = store(200, json!({ "value": { "held": false } })).await;
    let c = Client::new(base);
    assert!(!c.merge_beat(1, None, "T1", "ci").await.unwrap());
    let r = c
        .merge_result(
            1,
            None,
            "T1",
            &MergeOutcome::Updated {
                head: "h".into(),
                approved_intact: true,
            },
        )
        .await
        .unwrap();
    assert!(!r.held);
    c.merge_withdraw(1, None, "i1").await.unwrap();
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent[0]["args"], json!({ "token": "T1", "phase": "ci" }));
    assert_eq!(
        sent[1]["args"],
        json!({ "token": "T1", "outcome": "updated", "head": "h", "approvedIntact": true })
    );
    assert_eq!(sent[2]["args"], json!({ "item": "i1" }));

    let (base, _) = store(200, json!({ "value": { "turn": null } })).await;
    assert!(Client::new(base).merge_open(1, None).await.unwrap().is_none());
}

#[test]
fn every_outcome_has_its_wire_name() {
    let names: Vec<_> = [
        MergeOutcome::Merged { merge_commit: None },
        MergeOutcome::HandedOff,
        MergeOutcome::CiFailed { checks: json!([]) },
        MergeOutcome::SentBack { reason: "r".into() },
        MergeOutcome::NeedsPerson { reason: "r".into() },
        MergeOutcome::Closed,
        MergeOutcome::Retry { after_seconds: 5 },
        MergeOutcome::Released,
    ]
    .iter()
    .map(|o| o.wire()["outcome"].as_str().unwrap().to_string())
    .collect();
    assert_eq!(
        names,
        [
            "merged",
            "handed_off",
            "ci_failed",
            "sent_back",
            "needs_person",
            "closed",
            "retry",
            "released"
        ]
    );
}
