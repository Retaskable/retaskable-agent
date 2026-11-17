use super::*;
use crate::backlog::tests::store;

fn facts() -> CiFacts {
    CiFacts {
        pr: 7,
        sha: "abc1234".into(),
        title: "A title".into(),
        branch: "retaskable/7-x".into(),
        may_push: true,
        button: false,
        conflicting: false,
        checks: CiChecks {
            state: CiState::Pass,
            what: String::new(),
            logs: vec![],
            ended_at: None,
            nothing_reported: false,
        },
        pending_since: None,
        red_now: 0,
        base_head: None,
        did: None,
    }
}

#[test]
fn facts_are_camel_case_and_leave_out_what_is_unset() {
    let v = serde_json::to_value(facts()).unwrap();
    assert_eq!(
        v,
        json!({ "pr": 7, "sha": "abc1234", "title": "A title", "branch": "retaskable/7-x", "mayPush": true,
            "button": false, "conflicting": false, "redNow": 0,
            "checks": { "state": "pass", "what": "", "logs": [] } })
    );
}

#[test]
fn a_red_head_sends_its_checks_cut_to_the_limits() {
    let mut f = facts();
    f.title = "é".repeat(250);
    f.pending_since = Some(5);
    f.base_head = Some("deadbee".into());
    f.checks = CiChecks {
        state: CiState::Fail,
        what: "w".repeat(5000),
        logs: (0..12).map(|i| (i % 2 == 0).then(|| "l".repeat(4500))).collect(),
        ended_at: Some(99),
        nothing_reported: false,
    };
    let v = serde_json::to_value(&f).unwrap();
    assert_eq!(v["title"].as_str().unwrap().chars().count(), 200);
    assert_eq!(v["checks"]["state"], "fail");
    assert_eq!(v["checks"]["what"].as_str().unwrap().len(), 4000);
    let logs = v["checks"]["logs"].as_array().unwrap();
    assert_eq!(logs.len(), 10);
    assert_eq!(logs[0].as_str().unwrap().len(), 4000);
    assert!(logs[1].is_null());
    assert_eq!(
        (v["checks"]["endedAt"].clone(), v["pendingSince"].clone()),
        (json!(99), json!(5))
    );
    assert_eq!(v["baseHead"], "deadbee");
}

#[test]
fn did_has_the_shape_of_each_kind() {
    let did = |d: CiDid| serde_json::to_value(d).unwrap();
    assert_eq!(
        did(CiDid::Judge {
            verdict: Some(CiVerdict::NotChange),
            report: false
        }),
        json!({ "kind": "judge", "verdict": "not-change", "report": false })
    );
    assert_eq!(
        did(CiDid::Judge {
            verdict: None,
            report: true
        }),
        json!({ "kind": "judge", "verdict": null, "report": true })
    );
    assert_eq!(
        did(CiDid::Fix { pushed: true }),
        json!({ "kind": "fix", "pushed": true })
    );
    assert_eq!(
        did(CiDid::MergeMain { pushed: false }),
        json!({ "kind": "mergeMain", "pushed": false })
    );
    assert_eq!(
        did(CiDid::Rerun {
            rerun: CiRerun::Stuck,
            ok: true
        }),
        json!({ "kind": "rerun", "rerun": "stuck", "ok": true })
    );
    for (v, w) in [(CiVerdict::Change, "change"), (CiVerdict::Expectation, "expectation")] {
        assert_eq!(serde_json::to_value(v).unwrap(), w);
    }
}

#[test]
fn next_reads_each_instruction() {
    let next = |v: Value| serde_json::from_value::<CiNext>(v).unwrap();
    assert_eq!(next(json!({ "do": "judge" })), CiNext::Judge { report: false });
    assert_eq!(
        next(json!({ "do": "judge", "report": true })),
        CiNext::Judge { report: true }
    );
    assert_eq!(next(json!({ "do": "fix" })), CiNext::Fix);
    assert_eq!(next(json!({ "do": "mergeMain" })), CiNext::MergeMain);
    assert_eq!(
        next(json!({ "do": "rerun", "rerun": "force", "hung": false })),
        CiNext::Rerun {
            rerun: CiRerun::Force,
            hung: false
        }
    );
    assert_eq!(
        next(json!({ "do": "rerun", "rerun": "stuck", "hung": true })),
        CiNext::Rerun {
            rerun: CiRerun::Stuck,
            hung: true
        }
    );
    // A `do` it doesn't know is not an error: a newer store may say one.
    assert_eq!(next(json!({ "do": "rebase" })), CiNext::Unknown);
}

#[tokio::test]
async fn ci_result_sends_the_facts_and_reads_the_answer_ignoring_unknown_fields() {
    let (base, seen) = store(
        200,
        json!({ "value": { "ok": true, "condition": "red", "notTheChange": false, "found": true, "attempts": 1,
            "needsPerson": false, "next": { "do": "judge", "later": 1 }, "future": [1],
            "logs": [{ "pr": 7, "what": "boom", "bell": true, "extra": { "a": 1 } }],
            "bells": [{ "title": "Pull request #7 is red", "body": "A title (at abc1234)", "x": 0 }] } }),
    )
    .await;
    let mut f = facts();
    f.did = Some(CiDid::Fix { pushed: true });
    let a = Client::new(base).ci_result(3, Some("tok"), &f).await.unwrap();
    assert!(a.ok && a.found && !a.needs_person && !a.not_the_change);
    assert_eq!((a.condition, a.attempts), (CiCondition::Red, 1));
    assert_eq!(a.next, Some(CiNext::Judge { report: false }));
    assert_eq!(a.logs[0]["what"], "boom");
    assert_eq!(a.bells[0].title, "Pull request #7 is red");
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "ci_result");
    assert_eq!(sent["args"]["did"], json!({ "kind": "fix", "pushed": true }));
    assert_eq!(sent["args"]["mayPush"], true);
}

#[tokio::test]
async fn a_green_answer_has_no_next_and_nothing_to_write() {
    let (base, _) = store(
        200,
        json!({ "value": { "ok": true, "condition": "green", "next": null } }),
    )
    .await;
    let a = Client::new(base).ci_result(3, None, &facts()).await.unwrap();
    assert_eq!(
        (a.condition, a.next, a.logs.len(), a.bells.len()),
        (CiCondition::Green, None, 0, 0)
    );
}

#[tokio::test]
async fn a_refused_press_is_an_answer_with_its_words() {
    let (base, seen) = store(
        200,
        json!({ "value": { "ok": false, "refused": "Pull request #7 isn't one the watcher can fix.",
            "condition": "red", "notTheChange": false, "found": false, "attempts": 3, "needsPerson": true,
            "next": null, "logs": [], "bells": [] } }),
    )
    .await;
    let mut f = facts();
    f.button = true;
    let a = Client::new(base).ci_result(3, None, &f).await.unwrap();
    assert!(!a.ok && a.needs_person);
    assert_eq!(
        a.refused.as_deref(),
        Some("Pull request #7 isn't one the watcher can fix.")
    );
    assert_eq!(seen.lock().unwrap()[0]["args"]["button"], true);
}

#[tokio::test]
async fn ci_result_errors_keep_the_stores_code_and_an_unreadable_answer_is_one() {
    let (base, _) = store(
        400,
        json!({ "error": "bad_request", "message": "sha must be a commit id" }),
    )
    .await;
    let e = Client::new(base).ci_result(3, None, &facts()).await.unwrap_err();
    assert_eq!(e.code(), Some("bad_request"));
    let (base, _) = store(200, json!({ "value": { "ok": true } })).await;
    let e = Client::new(base).ci_result(3, None, &facts()).await.unwrap_err();
    assert!(matches!(e, BacklogError::BadAnswer { code: None, .. }));
    let e = Client::new("http://127.0.0.1:1")
        .ci_result(3, None, &facts())
        .await
        .unwrap_err();
    assert!(matches!(e, BacklogError::Unreachable(_)));
}

#[tokio::test]
async fn log_add_sends_the_kind_and_the_entry() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    let entry = json!({ "pr": 7, "what": "boom", "bell": true });
    Client::new(base)
        .log_add(3, Some("tok"), "ci", entry.as_object().unwrap())
        .await
        .unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "log_add");
    assert_eq!(sent["args"], json!({ "kind": "ci", "entry": entry }));
    let (base, _) = store(400, json!({ "error": "bad_request" })).await;
    let e = Client::new(base)
        .log_add(3, None, "nope", &Map::new())
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some("bad_request"));
}
