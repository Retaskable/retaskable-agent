//! A draft's final report (`merged` or `discarded`) is kept in the project's folder until the store answers, so a restart
//! in the middle of an outage sends it again before another draft is taken (the owner's rule, 2026-11-16: "it should be
//! able to finish what it's doing but it can't pick up the next thing till the store is back.").
use super::asks::{Listed, ops_named, push, store_failing};
use super::drafts::Host;
use super::merge_draft::{person, rig_draft, row};
use super::restart::restarted;
use super::*;
use crate::orchestrator::driver::draft_owed::{self, Owed};

fn owed(since: i64) -> Owed {
    Owed {
        lease: "lease-old".into(),
        status: "merged".into(),
        detail: Some("merged".into()),
        summary: Some("Merged the pull request".into()),
        since,
        ..Default::default()
    }
}

fn kept(rig: &Rig) -> std::collections::BTreeMap<String, Owed> {
    draft_owed::open(&rig._dir.path().join("0")).all()
}

fn next_draft() -> Value {
    json!({ "id": "d1", "request": "Add charts", "titles": ["Charts"] })
}

/// A computer that listed `owed` for draft `d0` and died with the store out, then started again against a store that
/// also has the next draft `d1` waiting. Draft passes are one pass at a time; `wait` is short so the test doesn't.
async fn restarted_with(owed: Owed) -> (Rig, Asked, Listed) {
    let (base, asked, listed) = store_failing("").await;
    listed.draft_claims.lock().unwrap().push(next_draft());
    let rig = rig(2, &[&base]);
    draft_owed::open(&rig._dir.path().join("0")).put("d0", owed);
    let mut rig = restarted(rig, &base);
    rig.go_live();
    rig.machine.drafts = Some(Arc::new(Host::default()));
    rig.lp(0).owed_wait = Duration::from_millis(1);
    (rig, asked, listed)
}

/// Passes until `done`; panics after 200.
async fn pass_until(rig: &mut Rig, what: &str, done: impl Fn(&Rig) -> bool) {
    for _ in 0..200 {
        rig.machine.pass().await;
        if done(rig) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never: {what}");
}

/// The reports of the draft that was owed (the next draft's own go beside them once it is claimed).
fn owed_reports(asked: &Asked) -> Vec<Value> {
    ops_named(asked, "draft_report")
        .into_iter()
        .filter(|a| a["id"] == "d0")
        .collect()
}

fn ops_of(asked: &Asked) -> Vec<String> {
    asked.lock().unwrap().iter().map(|(op, _)| op.clone()).collect()
}

#[tokio::test]
async fn a_restart_in_an_outage_sends_the_report_first_and_takes_no_draft_until_the_store_is_back() {
    let (mut rig, asked, listed) = restarted_with(owed(crate::owed_file::now_secs())).await;
    listed
        .down_for
        .lock()
        .unwrap()
        .insert("draft_report".into(), usize::MAX);
    // Out: the report is tried again and again, and nothing new is claimed.
    pass_until(&mut rig, "three refused sends", |_| owed_reports(&asked).len() >= 3).await;
    assert!(
        !ops_of(&asked).contains(&"draft_claim_next".to_string()),
        "{:?}",
        ops_of(&asked)
    );
    assert_eq!(kept(&rig).len(), 1, "still owed");
    // Back: it is taken, and then the next draft is claimed.
    listed.down_for.lock().unwrap().clear();
    pass_until(&mut rig, "the next draft is claimed", |_| {
        ops_of(&asked).contains(&"draft_claim_next".to_string())
    })
    .await;
    let sent = owed_reports(&asked).len();
    assert!(
        owed_reports(&asked)
            .iter()
            .all(|a| a["token"] == "lease-old" && a["status"] == "merged" && a["detail"] == "merged")
    );
    let last = asked
        .lock()
        .unwrap()
        .iter()
        .rposition(|(op, a)| op == "draft_report" && a["id"] == "d0");
    let claim = ops_of(&asked).iter().position(|o| o == "draft_claim_next");
    assert!(last < claim, "the report goes before the claim");
    assert!(kept(&rig).is_empty(), "off the disk once taken");
    // Taken once: it isn't sent again.
    for _ in 0..10 {
        rig.machine.pass().await;
        tokio::task::yield_now().await;
    }
    assert_eq!(owed_reports(&asked).len(), sent);
}

#[tokio::test]
async fn a_report_the_store_refuses_with_a_code_is_dropped_and_not_sent_again() {
    let (mut rig, asked, listed) = restarted_with(owed(crate::owed_file::now_secs())).await;
    listed
        .refused
        .lock()
        .unwrap()
        .insert("draft_report".into(), (409, "stale_lease"));
    pass_until(&mut rig, "the next draft is claimed", |_| {
        ops_of(&asked).contains(&"draft_claim_next".to_string())
    })
    .await;
    assert_eq!(owed_reports(&asked).len(), 1, "the draft moved on: asked once");
    assert!(kept(&rig).is_empty());
}

#[tokio::test]
async fn a_report_kept_more_than_a_day_is_tried_once_more_then_given_up() {
    let old = crate::owed_file::now_secs() - crate::owed_file::KEPT_SECS - 1;
    let (mut rig, asked, listed) = restarted_with(owed(old)).await;
    listed
        .down_for
        .lock()
        .unwrap()
        .insert("draft_report".into(), usize::MAX);
    pass_until(&mut rig, "the next draft is claimed", |_| {
        ops_of(&asked).contains(&"draft_claim_next".to_string())
    })
    .await;
    assert_eq!(owed_reports(&asked).len(), 1);
    assert!(kept(&rig).is_empty());
}

#[tokio::test]
async fn nothing_is_resent_when_nothing_is_owed() {
    let (base, asked, listed) = store_failing("").await;
    listed.draft_claims.lock().unwrap().push(next_draft());
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.drafts = Some(Arc::new(Host::default()));
    pass_until(&mut rig, "claimed", |_| {
        ops_of(&asked).contains(&"draft_claim_next".to_string())
    })
    .await;
    assert!(ops_named(&asked, "draft_report").iter().all(|a| a["id"] != "d0"));
}

#[tokio::test]
async fn a_merge_press_lists_its_report_on_disk_before_it_is_taken_and_removes_it_when_it_is() {
    let (mut rig, asked, listed, _host, _root) = rig_draft(
        json!({ "token": "lease-d", "draft": { "id": "d1", "status": "needs_person", "detail": "awaiting_approval",
            "titles": ["Charts"], "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1-charts",
            "itemIds": ["charts-1"] } }),
        person(),
        vec![],
    )
    .await;
    listed
        .down_for
        .lock()
        .unwrap()
        .insert("draft_report".into(), usize::MAX);
    push(&listed, row(json!({ "draftId": "d1" })));
    pass_until(&mut rig, "two refused sends", |_| {
        ops_named(&asked, "draft_report").len() >= 2
    })
    .await;
    let on_disk = kept(&rig);
    assert_eq!(on_disk["d1"].lease, "lease-d");
    assert_eq!(on_disk["d1"].status, "merged");
    listed.down_for.lock().unwrap().clear();
    pass_until(&mut rig, "taken", |rig| kept(rig).is_empty()).await;
}
