//! A person's answers to an item the session asked about (`driver/answers.rs`).
use super::*;
use crate::view::ItemCommand;

/// A live rig whose item "a" waits for answers to "Which?", parked at the store (no lane, no lease), with a store that hands
/// it back as claimed on `lease-2`.
async fn asked() -> (Rig, Arc<Fake>, Asked) {
    asked_with(item("a", "needs_person")).await
}

/// `asked`, with the store listing `listed` for the item.
async fn asked_with(listed: Value) -> (Rig, Arc<Fake>, Asked) {
    let claim = json!({ "item": item("a", "claimed"), "token": "lease-2" });
    let (base, asked) = claim_stub(json!([listed]), Some(claim)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::NeedsClarification;
    let lp = rig.lp(0);
    lp.records.insert(
        "a".into(),
        RunRecord {
            state: "needs_clarification".into(),
            impl_session_id: Some("s1".into()),
            branch: Some("a".into()),
            flow: crate::store::FlowRecord {
                questions: vec!["Which?".into()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    (rig, fake, asked)
}

fn answer(rig: &Rig, item: &str, answers: &[&str]) {
    rig.machine.links.item_commands.push(ItemCommand::Answer {
        project: "p0".into(),
        item: item.into(),
        answers: answers.iter().map(|a| a.to_string()).collect(),
        attached: None,
    });
}

fn reports(asked: &Asked) -> Vec<Value> {
    let asked = asked.lock().unwrap();
    (asked.iter())
        .filter(|(op, _)| op == "report")
        .map(|(_, a)| a.clone())
        .collect()
}

#[tokio::test]
async fn an_answer_claims_the_item_back_and_sends_the_implementer_the_questions_with_their_answers() {
    let (mut rig, fake, asked) = asked().await;
    answer(&rig, "a", &["Blue."]);
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    let said = started[0].follow_up.as_deref().unwrap();
    assert!(
        said.contains("Answers to your questions:\n\nQ: Which?\nA: Blue."),
        "{said}"
    );
    assert!(said.contains("Continue with the slice, then return the result."));
    drop(started);
    assert!(ops(&asked).contains(&"claim".to_string()), "{:?}", ops(&asked));
    let lp = rig.lp(0);
    assert_eq!(lp.runs["a"].state, ItemState::Implementing);
    assert_eq!(lp.leases.get("a").map(String::as_str), Some("lease-2"));
    assert!(lp.records["a"].flow.questions.is_empty() && lp.answering.is_empty());
    assert!(rig.machine.pool.lane_of("p0", "a").is_some());
    let noted: Vec<String> = reports(&asked)
        .iter()
        .filter_map(|r| r["note"].as_str().map(String::from))
        .collect();
    assert!(
        noted.contains(&"Answered by a person:\nQ: Which?\nA: Blue.".to_string()),
        "{noted:?}"
    );
}

#[tokio::test]
async fn an_answer_for_an_item_that_is_not_waiting_for_one_changes_nothing() {
    let (mut rig, fake, asked) = asked().await;
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Implementing;
    answer(&rig, "a", &["Blue."]);
    answer(&rig, "nope", &["x"]);
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).answering.is_empty() && fake.started.lock().unwrap().is_empty());
    assert!(!ops(&asked).contains(&"claim".to_string()));
}

#[tokio::test]
async fn an_answer_with_no_free_lane_waits_and_goes_on_a_later_pass() {
    let claim = json!({ "item": item("a", "claimed"), "token": "lease-2" });
    let (base, _asked) = claim_stub(json!([item("a", "needs_person")]), Some(claim)).await;
    let mut rig = rig(1, &[&base]);
    let fake = rig.go_live();
    rig.queue(0, &["a", "b"]);
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::NeedsClarification;
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    let held = rig.machine.pool.claim(&crate::lane_pool::Want {
        project: "p0".into(),
        item: "b".into(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    });
    assert!(held.is_some());
    answer(&rig, "a", &["Blue."]);
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsClarification);
    assert_eq!(rig.lp(0).answering.len(), 1);
    rig.machine.pool.release(
        held.unwrap(),
        &crate::lane_pool::Holder::Item {
            project: "p0".into(),
            item: "b".into(),
        },
    );
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).answering.is_empty() && fake.started.lock().unwrap().len() == 1);
}

#[tokio::test]
async fn with_no_questions_on_file_the_answers_are_the_persons_note() {
    let (mut rig, fake, _asked) = asked().await;
    rig.lp(0).records.get_mut("a").unwrap().flow.questions.clear();
    answer(&rig, "a", &["Just carry on."]);
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert!(
        started[0]
            .follow_up
            .as_deref()
            .unwrap()
            .contains("A person's note:\n\nJust carry on.")
    );
}

#[tokio::test]
async fn a_session_that_asks_keeps_its_questions_and_the_store_hears_them() {
    let asking = SessionResult::Asked(vec!["Which?".into(), "Why?".into()]);
    let (mut rig, _fake, asked) = super::super::settling::ended(asking, &Default::default()).await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsClarification);
    assert_eq!(rig.lp(0).records["a"].flow.questions, ["Which?", "Why?"]);
    let told: Vec<(Value, Value)> = reports(&asked)
        .iter()
        .map(|r| (r["status"].clone(), r["note"].clone()))
        .collect();
    assert!(
        told.contains(&(json!("needs_person"), json!("Questions for a person: Which? / Why?"))),
        "{told:?}"
    );
}

#[tokio::test]
async fn an_item_waiting_for_answers_is_parked_with_its_questions_and_survives_a_restart() {
    let asking = SessionResult::Asked(vec!["Which?".into()]);
    let (mut rig, _fake, _asked) = super::super::settling::ended(asking, &Default::default()).await;
    rig.machine.pass().await;
    assert_eq!(
        rig.lp(0).parked["a"],
        ("answer".to_string(), "needs_clarification".to_string())
    );
    let saved = Store::load(&rig._dir.path().join("0")).unwrap();
    assert_eq!(saved.runs()["a"].flow.questions, ["Which?"]);
    let (base, _) = claim_stub(json!([item("a", "needs_person")]), Some(Value::Null)).await;
    let mut rig = super::super::restart::restarted(rig, &base);
    rig.go_live();
    rig.machine.pass().await;
    let lp = rig.lp(0);
    assert_eq!(lp.runs["a"].state, ItemState::NeedsClarification);
    assert_eq!(lp.records["a"].flow.questions, ["Which?"]);
    assert!(lp.leases.is_empty() && lp.parked.contains_key("a"));
}

/// The needs_person item as the store lists it with a person's `decision` kept on it from the site.
fn decided(decision: Value) -> Value {
    let mut it = item("a", "needs_person");
    it["decision"] = decision;
    it
}

fn site_answer() -> Value {
    json!({ "kind": "answer", "answers": ["Blue."], "by": "ana", "at": 1 })
}

fn follow_up(fake: &Fake) -> Option<String> {
    fake.started.lock().unwrap().first().and_then(|s| s.follow_up.clone())
}

#[tokio::test]
async fn an_answer_kept_on_the_item_by_the_site_resumes_it_on_the_pass_that_reads_it() {
    let (mut rig, fake, asked) = asked_with(decided(site_answer())).await;
    rig.machine.pass().await;
    let said = follow_up(&fake).expect("the implementer was resumed");
    assert!(said.contains("Blue."), "{said}");
    assert!(ops(&asked).contains(&"claim".to_string()), "{:?}", ops(&asked));
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).answering.is_empty());
    // Read again (a stale read of the decision must not answer twice).
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_item_push_reads_the_items_waiting_for_an_answer_and_resumes_the_answered_one_at_once() {
    let (mut rig, fake, asked) = asked_with(decided(site_answer())).await;
    // No pass reads the board: only the push's wake and the poll that follows it.
    rig.machine.links.work.wake_for("p0", crate::work_signal::Wake::Changed);
    rig.machine.poll().await;
    assert!(follow_up(&fake).is_some_and(|s| s.contains("A: Blue.")));
    assert!(ops(&asked).contains(&"list".to_string()), "{:?}", ops(&asked));
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
}

#[tokio::test]
async fn without_a_push_a_poll_does_not_read_for_answers() {
    let (mut rig, fake, asked) = asked_with(decided(site_answer())).await;
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty() && !ops(&asked).contains(&"list".to_string()));
}

#[tokio::test]
async fn a_decision_that_is_not_an_answer_or_an_item_not_waiting_for_one_is_left_alone() {
    let tryout = json!({ "kind": "tryout", "result": "pass", "notes": "", "by": "ana", "at": 1 });
    let (mut rig, fake, _asked) = asked_with(decided(tryout)).await;
    rig.machine.pass().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsClarification);
    let (mut rig, fake, _asked) = asked_with(decided(site_answer())).await;
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Implementing;
    rig.machine.pass().await;
    assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).answering.is_empty());
}

#[tokio::test]
async fn a_restart_finds_an_answer_given_while_the_agent_was_down() {
    let asking = SessionResult::Asked(vec!["Which?".into()]);
    let (mut rig, _fake, _asked) = super::super::settling::ended(asking, &Default::default()).await;
    rig.machine.pass().await;
    let claim = json!({ "item": item("a", "claimed"), "token": "lease-2" });
    let (base, asked) = claim_stub(json!([decided(site_answer())]), Some(claim)).await;
    let mut rig = super::super::restart::restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    let said = follow_up(&fake).expect("resumed after the restart");
    assert!(said.contains("Q: Which?\nA: Blue."), "{said}");
    assert!(ops(&asked).contains(&"claim".to_string()));
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
}
