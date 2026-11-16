//! A person's answer to a try-out on a `runs` computer (`playtest_owed.rs`): the loop holds no item there, so the
//! answer goes to the store as `tryout_result`, and goes again until the store takes it.
use super::playtest_answers::tryout_store;
use super::*;
use crate::view::ItemCommand;

fn answer(rig: &Rig, id: &str, passed: bool, notes: Option<&str>) {
    rig.machine.links.item_commands.push(ItemCommand::Playtest {
        project: "p0".into(),
        id: id.into(),
        passed,
        notes: notes.map(Into::into),
        merged_at: None,
    });
}

async fn runs_rig_for(base: &str) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    rig.go_runs();
    rig
}

#[tokio::test]
async fn a_pass_sends_one_passed_result_and_a_fail_a_problem_with_the_notes() {
    let (base, results) = tryout_store(0).await;
    let mut rig = runs_rig_for(&base).await;
    answer(&rig, "7", true, None);
    rig.runs_pass().await;
    rig.runs_quiet().await;
    let sent = results.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0]["key"].as_str(), sent[0]["result"].as_str()),
        (Some("k1"), Some("passed"))
    );
    assert!(rig.lp(0).unanswered.is_empty());

    let (base, results) = tryout_store(0).await;
    let mut rig = runs_rig_for(&base).await;
    answer(&rig, "k1", false, Some("Broken."));
    rig.runs_pass().await;
    let sent = results.lock().unwrap().clone();
    assert_eq!(
        (sent[0]["result"].as_str(), sent[0]["notes"].as_str()),
        (Some("problem"), Some("Broken."))
    );
}

#[tokio::test]
async fn an_answer_the_store_refuses_goes_again_next_pass_and_only_once_after_it_is_taken() {
    let (base, results) = tryout_store(1).await;
    let mut rig = runs_rig_for(&base).await;
    answer(&rig, "7", true, None);
    rig.runs_pass().await;
    assert_eq!(results.lock().unwrap().len(), 1);
    assert!(rig.lp(0).unanswered.contains_key("7"));
    rig.runs_pass().await;
    assert_eq!(results.lock().unwrap().len(), 2);
    assert!(rig.lp(0).unanswered.is_empty());
    rig.runs_quiet().await;
    assert_eq!(results.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_id_with_no_waiting_tryout_sends_nothing_and_is_dropped() {
    let (base, results) = tryout_store(0).await;
    let mut rig = runs_rig_for(&base).await;
    answer(&rig, "nope", true, None);
    rig.runs_pass().await;
    assert!(results.lock().unwrap().is_empty());
    assert!(rig.lp(0).unanswered.is_empty());
}
