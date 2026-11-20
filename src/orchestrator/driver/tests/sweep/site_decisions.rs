//! What a person kept on the item from the site (`driver/site_answers.rs`): answers, try-outs, Start again and Carry on.
use super::answers::{asked_with, decided, follow_up, site_answer};
use super::*;

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "claim").count()
}

fn tryout(result: &str, notes: &str) -> Value {
    json!({ "kind": "tryout", "result": result, "notes": notes, "by": "ana", "at": 1 })
}

/// A rig whose item "a" is in `state` here, parked as `was` says (no lease), the store listing `listed` for it.
async fn parked_in(state: ItemState, was: (&str, &str), listed: Value) -> (Rig, Arc<Fake>, Asked) {
    let (mut rig, fake, asked) = asked_with(listed).await;
    let lp = rig.lp(0);
    lp.runs.get_mut("a").unwrap().state = state;
    lp.records.get_mut("a").unwrap().flow.questions.clear();
    lp.parked.insert("a".into(), (was.0.into(), was.1.into()));
    (rig, fake, asked)
}

#[tokio::test]
async fn an_answer_kept_for_an_item_that_moved_on_is_applied_once_and_does_not_loop() {
    // The store still shows it parked for an answer; here it went on to wait for CI (a restart reset it).
    let (mut rig, fake, asked) = parked_in(
        ItemState::AwaitingCi,
        ("answer", "needs_clarification"),
        decided(site_answer()),
    )
    .await;
    rig.machine.pass().await;
    let said = follow_up(&fake).expect("the answer was applied");
    assert!(said.contains("A person's note:") && said.contains("Blue."), "{said}");
    assert_eq!(claims(&asked), 1, "claimed back by name, so the store forgets it");
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    // The store hasn't cleared it yet on the next reads: nothing is applied or claimed again.
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(claims(&asked), 1);
    assert!(rig.lp(0).answering.is_empty());
}

#[tokio::test]
async fn a_try_out_that_passed_on_the_site_moves_the_item_on_and_claims_it_back() {
    let (mut rig, fake, asked) = parked_in(
        ItemState::AwaitingPlaytest,
        ("tryout", "awaiting_playtest"),
        decided(tryout("pass", "")),
    )
    .await;
    rig.machine.pass().await;
    let state = rig.lp(0).runs["a"].state;
    assert_ne!(state, ItemState::AwaitingPlaytest);
    assert!(fake.started.lock().unwrap().is_empty(), "a pass starts no session");
    assert_eq!(claims(&asked), 1, "{:?}", ops(&asked));
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
    assert_eq!(rig.lp(0).runs["a"].state, state);
}

#[tokio::test]
async fn a_try_out_problem_kept_on_the_site_resumes_the_implementer_with_the_notes() {
    let (mut rig, fake, asked) = parked_in(
        ItemState::AwaitingPlaytest,
        ("tryout", "awaiting_playtest"),
        decided(tryout("fail", "The button is blue.")),
    )
    .await;
    rig.machine.pass().await;
    let said = follow_up(&fake).expect("the implementer resumed");
    assert!(said.contains("The button is blue."), "{said}");
    assert_eq!(claims(&asked), 1);
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_try_out_kept_for_an_item_that_awaits_none_is_claimed_back_not_applied() {
    let (mut rig, fake, asked) = asked_with(decided(tryout("pass", ""))).await;
    rig.machine.pass().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsClarification);
    assert_eq!(claims(&asked), 1, "{:?}", ops(&asked));
    rig.machine.pass().await;
    assert_eq!(claims(&asked), 1);
}

fn sent_back(how: &str, notes: Option<&str>) -> Value {
    let mut it = item("a", "needs_person");
    it["sendBack"] = json!({ "how": how, "notes": notes, "by": "ana", "at": 2 });
    it
}

#[tokio::test]
async fn carry_on_kept_on_a_stopped_item_is_applied_as_continue_and_only_once() {
    let (base, _asked) = claim_stub(json!([sent_back("continue", Some("Use the other API."))]), None).await;
    let (mut rig, fake) = super::super::commands::stopped_at(2, Some("s1"), &base);
    rig.machine.pass().await;
    assert_eq!(follow_up(&fake).as_deref(), Some("Use the other API."));
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn start_again_kept_on_a_stopped_item_is_queued_as_start_over() {
    let (base, _asked) = claim_stub(json!([sent_back("retry", None)]), None).await;
    let (mut rig, _fake) = super::super::commands::stopped_at(2, Some("s1"), &base);
    rig.machine.pass().await;
    let lp = rig.lp(0);
    assert_eq!(lp.site_taken.get("a"), Some(&2), "taken once");
    assert!(lp.continuing.is_empty(), "not as a Continue");
}

#[tokio::test]
async fn an_answer_kept_on_an_item_stopped_here_with_no_questions_is_applied_once() {
    let (base, _asked) = claim_stub(json!([decided(site_answer())]), None).await;
    let (mut rig, fake) = super::super::commands::stopped_at(2, Some("s1"), &base);
    rig.machine.pass().await;
    let said = follow_up(&fake).expect("the answer was applied");
    assert!(said.contains("A person's note:") && said.contains("Blue."), "{said}");
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}
