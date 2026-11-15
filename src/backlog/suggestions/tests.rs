use super::*;
use crate::backlog::tests::store;

fn send(matches: Option<&str>) -> VerdictSend {
    VerdictSend {
        id: "s-1".into(),
        verdict: Verdict::NotPromising,
        reason: "Covered".into(),
        matches: matches.map(String::from),
    }
}

#[tokio::test]
async fn suggestions_ask_reads_them_in_order() {
    let (base, seen) = store(
        200,
        json!({ "value": [
            { "id": "a", "text": "Dark mode", "at": "2026-11-01T10:00:00Z" },
            { "id": "b", "text": "Charts", "at": "2026-11-02T10:00:00Z" } ] }),
    )
    .await;
    let got = Client::new(base).suggestions_ask(3, Some("t")).await.unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        (got[0].id.as_str(), got[0].text.as_str(), got[0].at.as_str()),
        ("a", "Dark mode", "2026-11-01T10:00:00Z")
    );
    assert_eq!(got[1].id, "b");
    assert_eq!(seen.lock().unwrap()[0]["op"], "suggestions_ask");
}

#[tokio::test]
async fn suggestion_verdict_sends_snake_case_and_matches_only_when_given() {
    let (base, seen) = store(200, json!({ "value": {} })).await;
    let c = Client::new(base);
    assert_eq!(
        c.suggestion_verdict(3, None, &send(None)).await.unwrap(),
        VerdictSent::Sent
    );
    let mut dup = send(Some("Dark theme"));
    dup.verdict = Verdict::Duplicate;
    c.suggestion_verdict(3, None, &dup).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "suggestion_verdict");
    assert_eq!(
        seen[0]["args"],
        json!({ "id": "s-1", "verdict": "not_promising", "reason": "Covered" })
    );
    assert_eq!(seen[1]["args"]["verdict"], "duplicate");
    assert_eq!(seen[1]["args"]["matches"], "Dark theme");
}

#[test]
fn every_verdict_has_its_wire_name() {
    let names: Vec<_> = [
        Verdict::Promising,
        Verdict::Duplicate,
        Verdict::NotPromising,
        Verdict::Spam,
    ]
    .iter()
    .map(|v| serde_json::to_value(v).unwrap())
    .collect();
    assert_eq!(
        names,
        vec![
            json!("promising"),
            json!("duplicate"),
            json!("not_promising"),
            json!("spam")
        ]
    );
}

#[tokio::test]
async fn an_already_answered_refusal_is_told_apart_but_other_failures_are_errors() {
    let (base, _) = store(409, json!({ "error": "already_answered" })).await;
    let r = Client::new(base).suggestion_verdict(3, None, &send(None)).await;
    assert_eq!(r.unwrap(), VerdictSent::AlreadyAnswered);
    let (base, _) = store(500, json!({ "error": "boom" })).await;
    assert!(
        Client::new(base)
            .suggestion_verdict(3, None, &send(None))
            .await
            .is_err()
    );
    assert!(
        Client::new("http://127.0.0.1:1")
            .suggestion_verdict(3, None, &send(None))
            .await
            .is_err()
    );
}
