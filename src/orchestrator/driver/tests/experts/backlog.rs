//! Handing an expert session to the backlog (`driver/experts/backlog.rs`): parked, a plan queued that revises it, a
//! handed session that takes no message, and the lease given back when the plan merges, before the store's `revise`.
use super::*;
use crate::backlog::revise::Owed;

/// The draft step's host, which the landing pass (and so the revisions it sends) runs under.
fn with_drafts(x: &mut Ex) {
    x.rig.machine.drafts = Some(Arc::new(super::super::drafts::Host::default()));
}

async fn handed(x: &mut Ex) {
    started(x).await;
    x.end("e1", ID, "backlog");
    let a = x.answer("e1").await;
    assert_eq!(a["type"], "ack", "{a}");
}

fn owed() -> Owed {
    Owed {
        item: ID.into(),
        remaining: "What is left".into(),
        title: "Fix it".into(),
        by: Some("ada".into()),
        since: 0,
    }
}

#[tokio::test]
async fn handing_to_the_backlog_parks_it_and_queues_a_plan_that_revises_it_with_the_conversation() {
    let mut x = ex().await;
    started(&mut x).await;
    x.end("e1", ID, "backlog");
    assert_eq!(x.answer("e1").await["type"], "ack");
    // Its work pushed first, so the plan can name the branch.
    assert_eq!(
        *x.host.parks.lock().unwrap(),
        vec!["retaskable/expert-fix-it-fix-it".to_string()]
    );
    let plan = &x.st.args("draft_queue")[0];
    assert_eq!(
        (
            plan["revises"].as_str(),
            plan["requestedBy"].as_str(),
            plan["assignee"].as_str()
        ),
        (Some(ID), Some("ada"), Some("ada"))
    );
    assert_eq!(plan["titles"], json!(["Fix it"]));
    let request = plan["request"].as_str().unwrap();
    assert!(
        request.starts_with("Fix it\n\nThis comes from an expert session ada drove;"),
        "{request}"
    );
    assert!(
        request.contains("pushed on `retaskable/expert-fix-it-fix-it`: build on that branch rather than start again.")
    );
    // The conversation, oldest first: the person's message, then the answer.
    assert!(request.ends_with("The person: Fix it\n\nDid: Fix it"), "{request}");
    let expert = x.expert(ID).unwrap();
    assert_eq!(
        (expert["detail"].as_str(), expert["plan"].as_str()),
        (Some("expert_handed"), Some("draft-1"))
    );
    assert_eq!(x.lane_of(ID), None);
    assert_eq!(x.st.count("release"), 0, "it keeps its lease until the plan merges");
}

#[tokio::test]
async fn a_handed_session_refuses_a_message_in_the_ts_words_and_runs_nothing() {
    let mut x = ex().await;
    handed(&mut x).await;
    x.say("m1", ID, "more please", false);
    let a = x.answer("m1").await;
    assert_eq!(a["type"], "nack");
    assert_eq!(
        a["reason"],
        "It's handed to the backlog: plan draft-1 revises it. Park it from Finish to drive it again."
    );
    // One the site wrote straight into the store's queue isn't run either.
    x.st.site_says(ID, "from the site");
    x.quiet().await;
    assert_eq!(x.jobs().len(), 1);
    assert_eq!(x.lane_of(ID), None);
}

#[tokio::test]
async fn parking_a_handed_session_takes_it_back() {
    let mut x = ex().await;
    handed(&mut x).await;
    x.end("e2", ID, "park");
    assert_eq!(x.answer("e2").await["type"], "ack");
    let expert = x.expert(ID).unwrap();
    assert_eq!(expert["detail"], "expert_parked");
    assert!(expert.get("plan").is_none());
    x.say("m1", ID, "back again", false);
    x.until("the turn after", |x| x.jobs().len() == 2).await;
}

#[tokio::test]
async fn a_plan_the_store_wont_queue_leaves_the_session_parked_with_the_reason() {
    let mut x = ex().await;
    started(&mut x).await;
    x.st.queue_refused.store(true, Ordering::SeqCst);
    x.end("e1", ID, "backlog");
    let a = x.answer("e1").await;
    assert_eq!(a["type"], "nack");
    assert!(
        a["reason"]
            .as_str()
            .unwrap()
            .starts_with("The backlog store wouldn't take the plan:"),
        "{a}"
    );
    assert_eq!(x.detail(ID).as_deref(), Some("expert_parked"));
}

#[tokio::test]
async fn when_its_plan_merges_the_lease_is_given_back_before_the_revision() {
    let mut x = ex().await;
    with_drafts(&mut x);
    handed(&mut x).await;
    x.rig.lp(0).owe_revision("draft-1", owed());
    x.until("the revision", |x| x.st.count("revise") == 1).await;
    let ops: Vec<String> = x.st.ops.lock().unwrap().iter().map(|(o, _)| o.clone()).collect();
    let at = |name: &str| ops.iter().rposition(|o| o == name).unwrap();
    assert!(at("report") < at("release") && at("release") < at("revise"), "{ops:?}");
    let note =
        x.st.args("report")
            .into_iter()
            .find(|a| a["note"] == "Handed to the backlog by ada");
    assert_eq!(note.unwrap()["token"], format!("lease-{ID}"));
    assert_eq!(x.st.args("revise")[0]["draftId"], "draft-1");
    assert!(x.expert(ID).is_none(), "gone from the snapshot: it goes on as any item");
    assert_eq!(x.st.count("expert_queue_clear"), 1);
    assert!(x.rig.lp(0).store.revise_owed().is_empty());
}

#[tokio::test]
async fn a_lease_the_store_wont_take_back_holds_the_revision_until_it_does() {
    let mut x = ex().await;
    with_drafts(&mut x);
    handed(&mut x).await;
    *x.st.release_refused.lock().unwrap() = Some("boom");
    x.rig.lp(0).owe_revision("draft-1", owed());
    x.until("the release tried", |x| x.st.count("release") >= 1).await;
    assert_eq!(x.st.count("revise"), 0, "the change would find it mid-slice");
    assert_eq!(x.detail(ID).as_deref(), Some("expert_handed"));
    *x.st.release_refused.lock().unwrap() = None;
    x.until("the revision", |x| x.st.count("revise") == 1).await;
    assert!(x.expert(ID).is_none());
}

#[tokio::test]
async fn a_lease_that_has_lapsed_is_the_stores_to_revise() {
    let mut x = ex().await;
    with_drafts(&mut x);
    handed(&mut x).await;
    *x.st.release_refused.lock().unwrap() = Some("stale_lease");
    x.rig.lp(0).owe_revision("draft-1", owed());
    x.until("the revision", |x| x.st.count("revise") == 1).await;
    assert!(x.expert(ID).is_none());
}

#[tokio::test]
async fn a_merged_plan_for_an_item_this_computer_doesnt_hold_just_revises_it() {
    let mut x = ex().await;
    with_drafts(&mut x);
    x.rig.lp(0).owe_revision("draft-1", owed());
    x.until("the revision", |x| x.st.count("revise") == 1).await;
    assert_eq!(x.st.count("release"), 0);
}

#[tokio::test]
async fn in_runs_mode_it_hands_off_and_gives_the_lease_back_too() {
    let mut x = ex().await;
    with_drafts(&mut x);
    x.rig.go_runs();
    x.start("s1", "Fix it");
    x.end("e1", ID, "backlog");
    x.rig.lp(0).owe_revision("draft-1", owed());
    for _ in 0..300 {
        x.rig.runs_pass().await;
        x.drain();
        if x.st.count("revise") == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(x.st.count("draft_queue"), 1);
    assert_eq!(x.st.count("release"), 1);
    assert_eq!(x.st.count("revise"), 1);
    assert!(x.expert(ID).is_none());
}
