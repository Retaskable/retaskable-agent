use super::*;
use crate::backlog::tests::store;

fn agent(tags: &[&str]) -> RunTakeAgent {
    RunTakeAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        engines: vec!["claude".into()],
        uses: RunTakeUses {
            plan: "claude".into(),
            build: "cursor".into(),
            fix: "cursor".into(),
            look: "claude".into(),
        },
        chores: None,
        week: None,
    }
}

#[test]
fn chores_off_is_sent_and_on_is_left_out() {
    let mut a = agent(&[]);
    assert!(serde_json::to_value(&a).unwrap().get("chores").is_none());
    a.chores = Some(false);
    assert_eq!(serde_json::to_value(&a).unwrap()["chores"], json!(false));
}

#[test]
fn the_chore_count_is_read_and_a_bad_one_ignored() {
    let read = |v: Value| ChoreCount::read(v.as_object().unwrap());
    assert_eq!(
        read(json!({ "chores": { "today": 2, "limit": 3 } })),
        Some(ChoreCount { today: 2, limit: 3 })
    );
    assert_eq!(read(json!({})), None);
    assert_eq!(read(json!({ "chores": "many" })), None);
    assert_eq!(read(json!({ "chores": { "today": -1, "limit": 3 } })), None);
    assert_eq!(read(json!({ "chores": { "today": 1 } })), None);
}

#[tokio::test]
async fn run_take_sends_kinds_and_reads_a_run() {
    let (base, seen) = store(
        200,
        json!({ "value": { "token": "t1", "kind": "build", "extra": 1,
            "piece": { "title": "T", "what": "W", "doneWhen": "D", "attempts": [{"k":1}],
                       "branch": "b", "change": {"n": 2}, "id": "p1" } } }),
    )
    .await;
    let c = Client::new(base);
    let a = c
        .run_take(
            1,
            Some("tok"),
            &agent(&[]),
            RunWant::Kinds(&[RunKind::Build, RunKind::Watch]),
        )
        .await
        .unwrap();
    let RunTakeAnswer::Run(run) = a else { panic!("{a:?}") };
    assert_eq!(run.token, "t1");
    assert_eq!(run.kind, RunKind::Build);
    assert_eq!(run.piece.done_when, "D");
    assert_eq!(run.piece.branch.as_deref(), Some("b"));
    assert_eq!(run.piece.attempts.len(), 1);
    assert_eq!(run.piece.other["id"], "p1");
    assert_eq!(run.other["extra"], 1);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "run_take");
    assert_eq!(sent["args"]["kinds"], json!(["build", "watch"]));
    assert!(sent["args"]["agent"].get("tags").is_none());
    assert_eq!(sent["args"]["agent"]["engines"], json!(["claude"]));
    assert_eq!(
        sent["args"]["agent"]["uses"],
        json!({ "plan": "claude", "build": "cursor", "fix": "cursor", "look": "claude" })
    );
    assert_eq!(sent["args"]["agent"]["baseBranch"], "main");
}

#[tokio::test]
async fn run_take_by_piece_sends_tags_when_there_are_some() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let a = Client::new(base)
        .run_take(1, None, &agent(&["gpu"]), RunWant::Piece("p1"))
        .await
        .unwrap();
    assert_eq!(a, RunTakeAnswer::Nothing);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["args"]["piece"], "p1");
    assert!(sent["args"].get("kinds").is_none());
    assert_eq!(sent["args"]["agent"]["tags"], json!(["gpu"]));
}

#[tokio::test]
async fn run_take_reads_waiting_for() {
    let (base, _) = store(200, json!({ "value": { "waitingFor": ["gpu"] } })).await;
    let a = Client::new(base)
        .run_take(1, None, &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap();
    assert_eq!(a, RunTakeAnswer::WaitingFor(json!(["gpu"])));
}

#[tokio::test]
async fn run_take_with_an_unreadable_run_is_an_error() {
    let (base, _) = store(200, json!({ "value": { "token": "t" } })).await;
    let e = Client::new(base)
        .run_take(1, None, &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap_err();
    assert!(matches!(e, BacklogError::BadAnswer { .. }));
}

#[tokio::test]
async fn run_beat_keeps_stop_apart_from_ok() {
    let (base, seen) = store(200, json!({ "value": { "a": "ok", "b": "stop" } })).await;
    let v = Client::new(base)
        .run_beat(1, None, &["a".to_string(), "b".to_string()])
        .await
        .unwrap();
    assert_eq!(v["a"], BeatVerdict::Ok);
    assert_eq!(v["b"], BeatVerdict::Stop);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "run_beat");
    assert_eq!(sent["args"]["tokens"], json!(["a", "b"]));
}

#[tokio::test]
async fn run_beat_reads_the_runs_the_store_wraps_its_verdicts_in() {
    let (base, _) = store(200, json!({ "value": { "runs": { "a": "ok", "b": "stop" } } })).await;
    let v = Client::new(base)
        .run_beat(1, None, &["a".into(), "b".into()])
        .await
        .unwrap();
    assert_eq!((v["a"], v["b"]), (BeatVerdict::Ok, BeatVerdict::Stop));
}

#[tokio::test]
async fn run_done_sends_outcome_and_result() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    let c = Client::new(base);
    c.run_done(1, None, "t", &RunOutcome::PrOpened, json!({ "branch": "b" }))
        .await
        .unwrap();
    c.run_done(1, None, "t", &RunOutcome::Failed("boom".into()), Value::Null)
        .await
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["op"], "run_done");
    assert_eq!(
        seen[0]["args"],
        json!({ "token": "t", "outcome": "pr_opened", "result": { "branch": "b" } })
    );
    assert_eq!(seen[1]["args"]["outcome"], "failed");
    assert_eq!(seen[1]["args"]["result"], json!({ "why": "boom" }));
}

#[tokio::test]
async fn run_done_tells_stale_run_from_other_refusals() {
    let (base, _) = store(409, json!({ "error": "stale_run", "message": "gone" })).await;
    let e = Client::new(base)
        .run_done(1, None, "t", &RunOutcome::Green, Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(e, RunDoneError::Stale));
    let (base, _) = store(400, json!({ "error": "bad_request" })).await;
    let e = Client::new(base)
        .run_done(1, None, "t", &RunOutcome::Green, Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(e, RunDoneError::Other(BacklogError::BadAnswer { .. })));
}

#[tokio::test]
async fn runs_drop_sends_the_op() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    Client::new(base).runs_drop(1, None).await.unwrap();
    assert_eq!(seen.lock().unwrap()[0]["op"], "runs_drop");
}

#[tokio::test]
async fn an_unreachable_store_is_unreachable() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let c = Client::new(format!("http://{addr}"));
    assert!(matches!(c.runs_drop(1, None).await, Err(BacklogError::Unreachable(_))));
    assert!(matches!(
        c.run_beat(1, None, &[]).await,
        Err(BacklogError::Unreachable(_))
    ));
    assert!(matches!(
        c.run_take(1, None, &agent(&[]), RunWant::Kinds(&[])).await,
        Err(BacklogError::Unreachable(_))
    ));
    assert!(matches!(
        c.run_done(1, None, "t", &RunOutcome::Green, Value::Null).await,
        Err(RunDoneError::Other(BacklogError::Unreachable(_)))
    ));
}

#[tokio::test]
async fn lock_take_sends_name_and_note_and_reads_a_token_or_a_holder() {
    let (base, seen) = store(200, json!({ "value": { "held": true, "token": "L1" } })).await;
    let c = Client::new(base);
    assert_eq!(
        c.lock_take(1, Some("tok"), "merge-main", Some("piece on box"))
            .await
            .unwrap(),
        LockTake::Held("L1".into())
    );
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "lock_take");
    assert_eq!(sent["args"], json!({ "name": "merge-main", "note": "piece on box" }));

    let (base, seen) = store(200, json!({ "value": { "held": false, "by": "other", "note": "n" } })).await;
    assert_eq!(
        Client::new(base).lock_take(1, None, "chore-tidy", None).await.unwrap(),
        LockTake::HeldBy {
            by: "other".into(),
            note: Some("n".into())
        }
    );
    assert!(seen.lock().unwrap()[0]["args"].get("note").is_none());
}

#[tokio::test]
async fn run_change_sends_the_token_branch_url_and_number() {
    let (base, seen) = store(200, json!({ "value": { "ok": true } })).await;
    Client::new(base)
        .run_change(1, Some("tok"), "R1", "retaskable/x", "https://h/o/r/pull/7", 7)
        .await
        .unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "run_change");
    assert_eq!(
        sent["args"],
        json!({ "token": "R1", "branch": "retaskable/x", "prUrl": "https://h/o/r/pull/7", "prNumber": 7 })
    );
}

#[tokio::test]
async fn lock_drop_sends_the_token() {
    let (base, seen) = store(200, json!({ "value": null })).await;
    Client::new(base).lock_drop(1, Some("tok"), "L1").await.unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(
        (sent["op"].as_str(), &sent["args"]),
        (Some("lock_drop"), &json!({ "token": "L1" }))
    );
}

#[tokio::test]
async fn a_refused_or_unreachable_store_is_an_error_for_both_lock_ops() {
    let (base, _) = store(403, json!({ "error": "forbidden", "message": "no" })).await;
    let c = Client::new(base);
    assert!(c.lock_take(1, None, "x", None).await.is_err());
    assert!(c.lock_drop(1, None, "L1").await.is_err());
    let c = Client::new("http://127.0.0.1:1");
    assert!(matches!(
        c.lock_take(1, None, "x", None).await,
        Err(BacklogError::Unreachable(_))
    ));
    assert!(matches!(
        c.lock_drop(1, None, "L1").await,
        Err(BacklogError::Unreachable(_))
    ));
    let (base, _) = store(200, json!({ "value": { "held": true } })).await;
    assert!(Client::new(base).lock_take(1, None, "x", None).await.is_err());
}

#[tokio::test]
async fn run_done_limited_sends_engine_and_until_when_known() {
    use chrono::TimeZone;
    let (base, seen) = store(200, json!({ "value": null })).await;
    let c = Client::new(base);
    let until = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 1, 0, 0).unwrap();
    let limited = |until| RunOutcome::Limited {
        engine: "claude".into(),
        until,
    };
    c.run_done(1, None, "t", &limited(Some(until)), json!({}))
        .await
        .unwrap();
    c.run_done(1, None, "t", &limited(None), json!({})).await.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0]["args"]["outcome"], "limited");
    assert_eq!(
        seen[0]["args"]["result"],
        json!({ "engine": "claude", "until": until.timestamp_millis() })
    );
    assert_eq!(seen[1]["args"]["result"], json!({ "engine": "claude" }));
}

#[test]
fn the_agent_serializes_engines_and_uses_in_camel_case_next_to_tags() {
    let v = serde_json::to_value(agent(&["gpu"])).unwrap();
    assert_eq!(v["tags"], json!(["gpu"]));
    assert_eq!(v["engines"], json!(["claude"]));
    assert_eq!(v["baseBranch"], "main");
    assert_eq!(
        v["uses"],
        json!({ "plan": "claude", "build": "cursor", "fix": "cursor", "look": "claude" })
    );
}

#[tokio::test]
async fn run_take_sends_the_week_and_reads_a_pass_over() {
    let (base, seen) = store(
        200,
        json!({ "value": { "passed": { "to": "box-2", "retryMs": 60000 } } }),
    )
    .await;
    let mut a = agent(&[]);
    a.week = Some(crate::backlog::week::WeekUse {
        used: 0.7,
        resets_at: 9,
    });
    let answer = Client::new(base)
        .run_take(1, Some("tok"), &a, RunWant::Kinds(&[RunKind::Build]))
        .await
        .unwrap();
    assert_eq!(
        answer,
        RunTakeAnswer::Passed(crate::backlog::week::Passed {
            to: "box-2".into(),
            retry_ms: 60_000
        })
    );
    assert_eq!(
        seen.lock().unwrap()[0]["args"]["agent"]["week"],
        json!({ "used": 0.7, "resetsAt": 9 })
    );
}

#[tokio::test]
async fn run_take_reads_a_plan_run_that_has_an_item_and_no_piece() {
    // The shape `start` in `shared/backlogPieceOps.ts` answers for a plan: `piece: null`, the item beside it.
    let (base, _) = store(
        200,
        json!({ "value": { "token": "t9", "kind": "plan",
            "item": { "id": "A1", "title": "Add search", "specRefs": ["docs/specs/search.md"], "branch": "retaskable/a1",
                      "looks": [{ "decision": "again", "instructions": "split it" }], "engine": "claude" },
            "piece": null } }),
    )
    .await;
    let a = Client::new(base)
        .run_take(1, Some("tok"), &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap();
    let RunTakeAnswer::Run(run) = a else { panic!("{a:?}") };
    assert_eq!(run.kind, RunKind::Plan);
    assert_eq!(run.piece.title, "Add search");
    assert_eq!(run.piece.other["id"], "A1");
    assert_eq!(run.piece.branch.as_deref(), Some("retaskable/a1"));
    assert_eq!(run.piece.other["specs"], json!(["docs/specs/search.md"]));
    assert_eq!(run.piece.other["looks"][0]["decision"], "again");
    assert_eq!(run.piece.other["engine"], "claude");
}

#[tokio::test]
async fn a_look_at_a_plan_keeps_the_item_history_on_the_run() {
    let (base, _) = store(
        200,
        json!({ "value": { "token": "t8", "kind": "look",
            "item": { "id": "A1", "title": "Add search", "history": ["Its plan failed"] }, "piece": null } }),
    )
    .await;
    let a = Client::new(base)
        .run_take(1, Some("tok"), &agent(&[]), RunWant::Kinds(&[RunKind::Look]))
        .await
        .unwrap();
    let RunTakeAnswer::Run(run) = a else { panic!("{a:?}") };
    assert_eq!(run.other["history"], json!(["Its plan failed"]));
}

#[tokio::test]
async fn a_run_with_no_piece_and_no_item_is_still_a_bad_answer() {
    for body in [
        json!({ "token": "t", "kind": "plan", "piece": null }),
        json!({ "token": "t", "kind": "build", "item": { "id": "A1" } }),
        json!({ "token": "t", "kind": "plan", "piece": null, "item": "A1" }),
        json!({ "token": "t", "kind": "build", "piece": 7 }),
    ] {
        let (base, _) = store(200, json!({ "value": body })).await;
        let e = Client::new(base)
            .run_take(1, Some("tok"), &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
            .await
            .unwrap_err();
        assert!(matches!(e, BacklogError::BadAnswer { .. }), "{e:?}");
    }
}

#[tokio::test]
async fn run_take_answers_nothing_for_a_null_value() {
    let (base, _) = store(200, json!({ "value": null })).await;
    let a = Client::new(base)
        .run_take(1, Some("tok"), &agent(&[]), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap();
    assert_eq!(a, RunTakeAnswer::Nothing);
}

#[tokio::test]
async fn run_take_names_a_piece_by_its_record_id_since_the_store_sends_no_piece_id() {
    // `start` in `shared/backlogPieceOps.ts`: the piece is `{ n, title, ... }` with no `id`, the item beside it.
    let (base, _) = store(
        200,
        json!({ "value": { "token": "t3", "kind": "build",
            "item": { "id": "idea-intent-rows", "title": "Intent rows: take rows", "specRefs": [], "branch": null },
            "piece": { "n": 2, "title": "Take draft_wanted rows", "what": "W", "doneWhen": "D" } } }),
    )
    .await;
    let a = Client::new(base)
        .run_take(1, Some("tok"), &agent(&[]), RunWant::Kinds(&[RunKind::Build]))
        .await
        .unwrap();
    let RunTakeAnswer::Run(run) = a else { panic!("{a:?}") };
    assert_eq!(run.piece.other["id"], "idea-intent-rows-piece-2");
    assert_eq!(run.piece.other["itemTitle"], "Intent rows: take rows");
    assert_eq!(run.piece.title, "Take draft_wanted rows");
}
