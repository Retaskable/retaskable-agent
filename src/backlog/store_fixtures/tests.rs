//! The client against what the real store answers and accepts (`tests/fixtures/store/answers.json`).
use super::*;
use crate::backlog::merge::{MergeNext, MergeOutcome};
use crate::backlog::pool::{Health, HealthAnswer};
use crate::backlog::runs::{BeatVerdict, RunKind, RunOutcome, RunTakeAgent, RunTakeAnswer, RunTakeUses, RunWant};
use crate::backlog::tests::store;
use crate::backlog::{ClaimNextAgent, Client};
use std::collections::BTreeMap;

/// This computer as `scripts/store-fixtures/generate.mts` names it.
fn run_agent() -> RunTakeAgent {
    RunTakeAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: vec![],
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

fn claim_agent() -> ClaimNextAgent {
    ClaimNextAgent {
        machine: "box".into(),
        environment: "Production".into(),
        base_branch: "main".into(),
        tags: vec![],
        week: None,
    }
}

/// The request the client sent to the fake store, as the first one seen.
fn request(seen: &crate::backlog::tests::Seen) -> Value {
    seen.lock().unwrap()[0]["args"].clone()
}

#[test]
fn the_file_has_a_source_and_the_ops_the_agent_uses() {
    let file = file();
    assert_eq!(
        file["source"]["commit"].as_str().map(str::len),
        Some(40),
        "the Retaskable.com commit it came from"
    );
    let ops: std::collections::BTreeSet<String> = names().iter().map(|n| case(n).op).collect();
    for op in [
        "run_take",
        "run_beat",
        "run_done",
        "merge_next",
        "merge_beat",
        "merge_result",
        "merge_open",
        "merge_enqueue",
        "merge_withdraw",
        "health",
        "settings_get",
        "intent_list",
        "intent_claim",
        "intent_beat",
        "intent_finish",
        "draft_claim_next",
        "ci_claim",
        "ci_finish",
    ] {
        assert!(ops.contains(op), "no fixture of {op}");
    }
}

#[tokio::test]
async fn run_take_reads_each_run_the_store_hands_out() {
    for (name, kind, want) in [
        ("run_take_plan", RunKind::Plan, None),
        ("run_take_piece_build", RunKind::Build, Some("store#1")),
        ("run_take_piece_watch", RunKind::Watch, Some("store#1")),
    ] {
        let (base, seen) = store(200, answer(name)).await;
        let kinds = [kind];
        let want_arg = want.map_or(RunWant::Kinds(&kinds), RunWant::Piece);
        let got = Client::new(base)
            .run_take(1, None, &run_agent(), want_arg)
            .await
            .unwrap();
        let RunTakeAnswer::Run(run) = got else {
            panic!("{name}: {got:?}")
        };
        assert_eq!(run.kind, kind, "{name}");
        assert_eq!(run.token, value(name)["token"].as_str().unwrap());
        if want.is_some() {
            assert_eq!(run.piece.title, "Piece 1", "{name}: the piece's own fields");
        }
        assert_sent(name, &request(&seen));
    }
    let (base, _) = store(200, answer("run_take_nothing_for_plan_when_busy")).await;
    let got = Client::new(base)
        .run_take(1, None, &run_agent(), RunWant::Kinds(&[RunKind::Plan]))
        .await
        .unwrap();
    assert!(matches!(got, RunTakeAnswer::Nothing));
}

#[tokio::test]
async fn run_beat_reads_the_verdicts() {
    let (base, seen) = store(200, answer("run_beat")).await;
    let got = Client::new(base).run_beat(1, None, &["t1".to_string()]).await.unwrap();
    assert_eq!(got.get("t1"), Some(&BeatVerdict::Ok));
    assert_sent("run_beat", &request(&seen));
}

#[tokio::test]
async fn run_done_sends_what_the_store_took() {
    for (name, outcome) in [
        ("run_done_planned", RunOutcome::Planned),
        ("run_done_pr_opened", RunOutcome::PrOpened),
        ("run_done_green", RunOutcome::Green),
    ] {
        let case = case(name);
        let (base, seen) = store(200, answer(name)).await;
        let token = case.args["token"].as_str().unwrap();
        let result = case.args.get("result").cloned().unwrap_or_else(|| json!({}));
        let args = crate::backlog::runs::run_done_args(token, &outcome, result);
        Client::new(base)
            .run_done(1, None, token, &outcome, args["result"].clone())
            .await
            .unwrap();
        assert_sent(name, &request(&seen));
        assert_eq!(args["outcome"], case.args["outcome"], "{name}");
    }
}

#[tokio::test]
async fn merge_next_reads_every_answer_of_the_store() {
    // A piece's turn: the piece is under `taken`.
    let (base, seen) = store(200, answer("merge_next_piece")).await;
    let got = Client::new(base)
        .merge_next(1, None, "main", Some("my-pc"))
        .await
        .unwrap();
    let MergeNext::Turn(turn) = got else { panic!("{got:?}") };
    let piece = turn.piece.as_ref().expect("the piece, from turn.taken.piece");
    assert_eq!(piece.title, "Piece 1");
    assert_eq!(piece.branch.as_deref(), Some("retaskable/store-piece-1"));
    assert_eq!(turn.token, value("merge_next_piece")["turn"]["token"].as_str().unwrap());
    assert_eq!(turn.lease_ms, 300_000);
    assert_eq!(
        turn.other["item"]["id"], "store",
        "taken's item is laid in as a run's is"
    );
    assert_sent("merge_next_piece", &request(&seen));

    // An item step's turn has no `taken`.
    let (base, seen) = store(200, answer("merge_next_item")).await;
    let got = Client::new(base).merge_next(1, None, "main", None).await.unwrap();
    let MergeNext::Turn(turn) = got else { panic!("{got:?}") };
    assert!(turn.piece.is_none());
    assert_eq!(turn.entry["item"], "store");
    assert_sent("merge_next_item", &request(&seen));

    let (base, _) = store(200, answer("merge_next_piece_waiting")).await;
    let got = Client::new(base)
        .merge_next(1, None, "main", Some("other-pc"))
        .await
        .unwrap();
    assert_eq!(
        got,
        MergeNext::Waiting {
            place: 1,
            ahead: Some("store".into())
        }
    );
    let (base, _) = store(200, answer("merge_next_nothing")).await;
    let got = Client::new(base)
        .merge_next(1, None, "main", Some("my-pc"))
        .await
        .unwrap();
    assert_eq!(got, MergeNext::Nothing);
}

#[tokio::test]
async fn merge_beat_and_merge_open_read_the_store() {
    for (name, held) in [("merge_beat_held", true), ("merge_beat_lost", false)] {
        let args = case(name).args;
        let (base, seen) = store(200, answer(name)).await;
        let got = Client::new(base)
            .merge_beat(
                1,
                None,
                args["token"].as_str().unwrap(),
                args["phase"].as_str().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(got, held, "{name}");
        assert_sent(name, &request(&seen));
    }
    let (base, seen) = store(200, answer("merge_open_held")).await;
    let turn = Client::new(base)
        .merge_open(1, None)
        .await
        .unwrap()
        .expect("a held turn");
    assert_eq!(turn.piece.as_ref().map(|p| p.title.as_str()), Some("Piece 1"));
    assert_sent("merge_open_held", &request(&seen));
    let (base, _) = store(200, answer("merge_open_none")).await;
    assert!(Client::new(base).merge_open(1, None).await.unwrap().is_none());
}

#[tokio::test]
async fn merge_result_sends_what_the_store_takes_and_reads_its_answer() {
    let limit = limit("merge_result_reason");
    assert_eq!(limit, 300, "the store's limit on a reason, as the client's REASON_MAX");
    // A reason of any length is cut to the store's limit, in UTF-16 units (an emoji is two).
    for reason in [
        "x".repeat(1000),
        "🙂".repeat(400),
        format!("  {}  ", "y".repeat(299)) + "🙂",
    ] {
        let (base, seen) = store(200, answer("merge_result_sent_back")).await;
        let out = MergeOutcome::SentBack { reason };
        Client::new(base).merge_result(1, None, "t", &out).await.unwrap();
        let body = request(&seen);
        let units = body["reason"].as_str().unwrap().encode_utf16().count();
        assert!(
            units <= limit,
            "a reason of {units} units: the store refuses over {limit}"
        );
    }
    let (base, seen) = store(200, answer("merge_result_sent_back")).await;
    let out = MergeOutcome::SentBack {
        reason: "x".repeat(300),
    };
    let got = Client::new(base).merge_result(1, None, "t1", &out).await.unwrap();
    assert!(!got.held);
    assert_eq!(got.next, Some(json!("store")));
    let mut body = request(&seen);
    body["token"] = json!(case("merge_result_sent_back").args["token"]);
    assert_sent("merge_result_sent_back", &body);
    // The store refuses 301.
    assert_eq!(
        case("merge_result_reason_too_long").error.as_deref(),
        Some("bad_request")
    );

    let (base, seen) = store(200, answer("merge_result_merged")).await;
    let sha = case("merge_result_merged").args["mergeCommit"]
        .as_str()
        .unwrap()
        .to_string();
    let out = MergeOutcome::Merged {
        merge_commit: Some(sha),
    };
    assert!(!Client::new(base).merge_result(1, None, "t", &out).await.unwrap().held);
    let mut body = request(&seen);
    body["token"] = json!(case("merge_result_merged").args["token"]);
    assert_sent("merge_result_merged", &body);
}

#[tokio::test]
async fn merge_enqueue_and_withdraw_send_what_the_store_took() {
    let (base, seen) = store(200, answer("merge_enqueue_item")).await;
    let entry = crate::backlog::merge::MergeEntry {
        item: "store",
        base: "main",
        head: "h1",
        pr: crate::backlog::merge::MergePr {
            number: 7,
            url: "https://github.com/o/r/pull/7",
        },
        land: "retaskable",
        holds_branch: true,
        ..Default::default()
    };
    let v = Client::new(base).merge_enqueue(1, None, &entry).await.unwrap();
    assert_eq!(v["place"]["place"], 1);
    assert_sent("merge_enqueue_item", &request(&seen));
    let (base, seen) = store(200, answer("merge_withdraw")).await;
    Client::new(base).merge_withdraw(1, None, "store").await.unwrap();
    assert_sent("merge_withdraw", &request(&seen));
}

#[tokio::test]
async fn intent_rows_are_read_and_their_requests_match() {
    let (base, seen) = store(200, answer("intent_list")).await;
    let rows = Client::new(base).intent_list(1, None, &["ask"]).await.unwrap();
    assert_eq!(
        (rows.len(), rows[0].kind.as_str(), rows[0].status.as_str()),
        (1, "ask", "requested")
    );
    assert!(rows[0].open());
    assert_eq!(rows[0].issued_by.as_deref(), Some("alice"));
    assert_eq!(rows[0].args["question"], "Show me the pricing page");
    assert_sent("intent_list", &request(&seen));
    let (base, _) = store(200, answer("intent_list_none")).await;
    assert!(
        Client::new(base)
            .intent_list(1, None, &["nothing"])
            .await
            .unwrap()
            .is_empty()
    );

    let id = value("intent_request")["id"].as_str().unwrap().to_string();
    let (base, seen) = store(200, answer("intent_claim")).await;
    let claim = Client::new(base).intent_claim(1, None, &id, "my-pc").await.unwrap();
    assert!(claim.won && !claim.blocked);
    assert_sent("intent_claim", &request(&seen));
    let (base, seen) = store(200, answer("intent_beat")).await;
    assert!(Client::new(base).intent_beat(1, None, &id).await.unwrap());
    assert_sent("intent_beat", &request(&seen));
    let (base, seen) = store(200, answer("intent_finish")).await;
    let result = case("intent_finish").args["result"].clone();
    let ok = Client::new(base)
        .intent_finish(1, None, &id, crate::backlog::intents::Ending::Done, Some(&result), None)
        .await
        .unwrap();
    assert!(ok);
    assert_sent("intent_finish", &request(&seen));
}

#[tokio::test]
async fn draft_claim_next_reads_a_claimed_draft() {
    let (base, seen) = store(200, answer("draft_claim_next")).await;
    let claim = Client::new(base)
        .draft_claim_next(1, None, &claim_agent(), &["claude".to_string()])
        .await
        .unwrap()
        .expect("a claimed draft");
    assert_eq!(claim.draft.titles, ["Dark mode"]);
    assert_eq!(claim.draft.request.as_deref(), Some("Make it dark"));
    assert_eq!(claim.token, value("draft_claim_next")["token"].as_str().unwrap());
    assert_sent("draft_claim_next", &request(&seen));
    let (base, _) = store(200, answer("draft_claim_next_none")).await;
    let none = Client::new(base)
        .draft_claim_next(1, None, &claim_agent(), &[])
        .await
        .unwrap();
    assert!(none.is_none());
}

#[tokio::test]
async fn health_sends_what_the_store_takes_and_reads_its_tasks() {
    let report = Health {
        obeys: true,
        can: vec!["repo".into(), "engine".into()],
        project: "p".into(),
        agent: claim_agent(),
        running: vec![],
        usage: None,
        limits: BTreeMap::new(),
        spend: BTreeMap::new(),
        refused: None,
        paused: false,
        draining: false,
        own_failure: None,
        main: None,
        open_changes: None,
    };
    let (base, seen) = store(200, answer("health")).await;
    let got: HealthAnswer = Client::new(base).health(1, None, &report).await.unwrap();
    assert_sent("health", &request(&seen));
    let tasks = value("health")["tasks"].as_array().unwrap().len();
    assert_eq!(got.tasks.len(), tasks, "every task the store sent was read");
    assert_eq!(got.tasks[0].id, "task-1");
}

#[test]
fn settings_get_is_read_and_the_tests_stand_in_has_its_fields() {
    let saved = value("settings_get_saved");
    let effective = crate::store_effective::StoreEffective::read(&saved).expect("the effective settings it sends");
    let _ = effective;
    assert!(crate::store_effective::StoreEffective::read(&value("settings_get_empty")).is_some());
    // The tests' hand-written stand-in has no field the real store doesn't send (it may leave out ones the agent doesn't read).
    let keys = |v: &Value| -> std::collections::BTreeSet<String> { v.as_object().unwrap().keys().cloned().collect() };
    let real = keys(&saved["effective"]);
    for key in keys(&crate::store_effective::StoreEffective::test_json()) {
        assert!(
            real.contains(&key),
            "StoreEffective::test_json has {key}, which the store's `effective` doesn't"
        );
    }
    assert_eq!(saved["values"]["baseBranch"], "dev");
}

/// With `RETASKABLE_COM_DIR` set (a checkout with `npm ci --ignore-scripts` run), the file is regenerated from that
/// checkout's store code and must match the committed one apart from the commit it names. Without it the test says
/// so and passes, so `cargo test` needs no Node and no network.
#[test]
fn the_file_is_what_the_store_code_gives() {
    let Some(dir) = std::env::var_os("RETASKABLE_COM_DIR").filter(|d| !d.is_empty()) else {
        eprintln!(
            "skipped: set RETASKABLE_COM_DIR to a Retaskable.com checkout to regenerate tests/fixtures/store/answers.json and compare"
        );
        return;
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = std::env::temp_dir().join(format!("store-answers-{}.json", std::process::id()));
    let status = std::process::Command::new("node")
        .args(["--import", "tsx"])
        .arg(root.join("scripts/store-fixtures/generate.mts"))
        .arg(&out)
        .env("RETASKABLE_COM_DIR", &dir)
        .current_dir(&dir)
        .status()
        .expect("node runs");
    assert!(status.success(), "the generator failed");
    let fresh: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let _ = std::fs::remove_file(&out);
    let kept = file();
    assert_eq!(
        (&fresh["limits"], &fresh["cases"]),
        (&kept["limits"], &kept["cases"]),
        "the store's answers changed since commit {}: run scripts/store-fixtures/generate.sh and review the diff",
        kept["source"]["commit"]
    );
}
