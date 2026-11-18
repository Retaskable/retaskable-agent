//! A plan a person merged (or closed) on the code host while it waited for approval (`driver/land.rs`, `settle_landed`;
//! the TS agent's `draftsLanded`): the store's draft list names plans waiting for approval that no computer holds, a fake
//! host says where each pull request stands, and the plan ends as one this agent merged does.
use super::asks::{Listed, ops_named, quiet_passes};
use super::merge_draft::{person, rig_draft};
use super::restart::restarted;
use super::revisions::merging;
use super::*;
use crate::drafts::land::PrState;
use crate::orchestrator::driver::draft_owed::{self, Owed};

/// The store's list entry of the plan: waiting for approval with pull request 9, held by `held_by`.
fn listed_plan(held_by: Value) -> Value {
    json!({ "id": "d1", "status": "needs_person", "detail": "awaiting_approval", "titles": ["Gradual rollout"],
        "prUrl": "https://github.com/o/r/pull/9", "heldBy": held_by })
}

/// A live project whose store lists the plan (let go) and hands it to whoever takes it; pull request 9 is `state`.
async fn rig_landed(state: PrState, taken: Value) -> (Rig, Asked, Listed, Arc<approve::Host>, tempfile::TempDir) {
    let (rig, asked, listed, host, root) = rig_draft(taken, person(), vec![]).await;
    listed.drafts.lock().unwrap().push(listed_plan(Value::Null));
    host.states.lock().unwrap().insert(9, state);
    (rig, asked, listed, host, root)
}

/// Passes until `done`; panics after 200.
async fn pass_until(rig: &mut Rig, what: &str, done: impl Fn() -> bool) {
    for _ in 0..200 {
        rig.machine.pass().await;
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("never: {what}");
}

fn reports(asked: &Asked) -> Vec<Value> {
    ops_named(asked, "draft_report")
}

#[tokio::test]
async fn a_plan_merged_on_the_code_host_is_reported_merged_and_revises_its_item() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, _listed, host, _root) = rig_landed(PrState::Merged, taken).await;
    pass_until(&mut rig, "the item is revised", || {
        !ops_named(&asked, "revise").is_empty()
    })
    .await;
    // Taken first (the lock), then told merged under the lease it handed out; nothing merged by us.
    assert_eq!(ops_named(&asked, "draft_take")[0]["id"], "d1");
    let report = &reports(&asked)[0];
    assert_eq!(
        (
            report["status"].as_str(),
            report["detail"].as_str(),
            report["token"].as_str()
        ),
        (Some("merged"), Some("merged"), Some("lease-d"))
    );
    assert_eq!(host.calls(), ["remove d1", "fetch"]);
    assert_eq!(
        ops_named(&asked, "revise")[0],
        json!({ "id": "rollout", "remaining": "Roll out every promotion", "draftId": "d1", "by": "ana", "title": "Gradual rollout" })
    );
    // Its items' headers go to the store as an approved plan's do.
    assert_eq!(ops_named(&asked, "upsert_from_specs")[0]["approved"], json!(["alerts"]));
    let ops: Vec<String> = asked.lock().unwrap().iter().map(|(o, _)| o.clone()).collect();
    let at = |name: &str| ops.iter().position(|o| o == name).unwrap();
    assert!(at("draft_take") < at("draft_report") && at("draft_report") < at("revise"));
    // Once: the store no longer lists it, and nothing is sent again.
    quiet_passes(&mut rig).await;
    assert_eq!((reports(&asked).len(), ops_named(&asked, "revise").len()), (1, 1));
}

#[tokio::test]
async fn in_runs_mode_a_plan_merged_on_the_code_host_revises_its_item_too() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, _listed, _host, _root) = rig_landed(PrState::Merged, taken).await;
    rig.go_runs();
    for _ in 0..200 {
        rig.runs_pass().await;
        if !ops_named(&asked, "revise").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(reports(&asked)[0]["status"], "merged");
    assert_eq!(ops_named(&asked, "revise")[0]["draftId"], "d1");
}

#[tokio::test]
async fn a_plan_merged_without_a_revision_revises_nothing() {
    let mut taken = merging(json!({ "remaining": "x" }));
    taken["draft"]["revision"] = Value::Null;
    let (mut rig, asked, _listed, _host, _root) = rig_landed(PrState::Merged, taken).await;
    pass_until(&mut rig, "merged", || !reports(&asked).is_empty()).await;
    quiet_passes(&mut rig).await;
    assert!(ops_named(&asked, "revise").is_empty());
}

#[tokio::test]
async fn a_plan_whose_pull_request_was_closed_is_discarded() {
    let taken = merging(json!({ "remaining": "x" }));
    let (mut rig, asked, _listed, host, _root) = rig_landed(PrState::Closed, taken).await;
    pass_until(&mut rig, "discarded", || !reports(&asked).is_empty()).await;
    let report = &reports(&asked)[0];
    assert_eq!(
        (report["status"].as_str(), report["detail"].as_str()),
        (Some("discarded"), Some("discarded"))
    );
    // Already closed: it is not closed again, and nothing is fetched or revised.
    assert_eq!(host.calls(), ["remove d1"]);
    assert!(ops_named(&asked, "revise").is_empty());
}

#[tokio::test]
async fn an_open_one_a_held_one_and_one_not_waiting_for_approval_are_left_alone() {
    let (mut rig, asked, listed, host, _root) = rig_landed(PrState::Merged, merging(json!({ "remaining": "x" }))).await;
    {
        let mut rows = listed.drafts.lock().unwrap();
        rows.clear();
        rows.push(listed_plan(json!({ "machine": "other-pc" })));
        let mut working = listed_plan(Value::Null);
        (working["status"], working["detail"]) = (json!("running"), json!("drafting"));
        rows.push(working);
        let mut open = listed_plan(Value::Null);
        open["prUrl"] = json!("https://github.com/o/r/pull/12");
        rows.push(open);
    }
    pass_until(&mut rig, "looked", || !host.looked.lock().unwrap().is_empty()).await;
    quiet_passes(&mut rig).await;
    // Only the one open plan's pull request was read; nothing was taken or reported.
    assert_eq!(*host.looked.lock().unwrap(), [12]);
    assert!(ops_named(&asked, "draft_take").is_empty() && reports(&asked).is_empty());
}

#[tokio::test]
async fn the_plans_are_looked_at_every_so_often_not_every_pass() {
    let (mut rig, asked, _listed, host, _root) = rig_landed(PrState::Open, merging(json!({ "remaining": "x" }))).await;
    pass_until(&mut rig, "looked", || !host.looked.lock().unwrap().is_empty()).await;
    host.states.lock().unwrap().insert(9, PrState::Merged);
    quiet_passes(&mut rig).await;
    assert_eq!(
        (host.looked.lock().unwrap().len(), reports(&asked).len()),
        (1, 0),
        "too soon to look again"
    );
    rig.lp(0).landing.look_every = Duration::ZERO;
    pass_until(&mut rig, "merged", || !reports(&asked).is_empty()).await;
    assert_eq!(reports(&asked)[0]["status"], "merged");
}

#[tokio::test]
async fn with_the_store_out_nothing_is_taken_and_the_plan_is_looked_at_once_it_is_back() {
    let (mut rig, asked, listed, host, _root) = rig_landed(PrState::Merged, merging(json!({ "remaining": "x" }))).await;
    listed.down_for.lock().unwrap().insert("drafts".into(), 3);
    pass_until(&mut rig, "asked three times", || ops_named(&asked, "drafts").len() >= 3).await;
    assert!(host.looked.lock().unwrap().is_empty() && ops_named(&asked, "draft_take").is_empty());
    pass_until(&mut rig, "merged", || !reports(&asked).is_empty()).await;
    assert_eq!(reports(&asked)[0]["status"], "merged");
}

#[tokio::test]
async fn a_merged_report_the_store_cant_take_is_kept_and_sent_when_it_is_back_also_after_a_restart() {
    let (mut rig, asked, listed, _host, _root) =
        rig_landed(PrState::Merged, merging(json!({ "remaining": "x" }))).await;
    listed
        .down_for
        .lock()
        .unwrap()
        .insert("draft_report".into(), usize::MAX);
    let kept = |rig: &Rig| draft_owed::open(&rig._dir.path().join("0")).all();
    pass_until(&mut rig, "kept", || ops_named(&asked, "draft_report").len() >= 2).await;
    let owed: Owed = kept(&rig)["d1"].clone();
    assert_eq!((owed.lease.as_str(), owed.status.as_str()), ("lease-d", "merged"));
    // The computer dies in the outage (its sending task with it) and starts again with the store back.
    for task in rig.lp(0).landing.tasks.drain(..) {
        task.abort();
    }
    let sent = reports(&asked).len();
    listed.down_for.lock().unwrap().clear();
    let (base, asked2, listed2) = super::asks::store_failing("").await;
    let _ = listed;
    let mut rig = restarted(rig, &base);
    rig.go_live();
    rig.machine.drafts = Some(Arc::new(approve::Host::default()));
    pass_until(&mut rig, "sent again", || {
        !ops_named(&asked2, "draft_report").is_empty()
    })
    .await;
    let again = &ops_named(&asked2, "draft_report")[0];
    assert_eq!(
        (again["id"].as_str(), again["token"].as_str(), again["status"].as_str()),
        (Some("d1"), Some("lease-d"), Some("merged"))
    );
    assert!(sent >= 2 && listed2.drafts.lock().unwrap().is_empty());
    // The revision kept with the report follows it, once, and the entry goes only after it is listed.
    pass_until(&mut rig, "revised", || !ops_named(&asked2, "revise").is_empty()).await;
    quiet_passes(&mut rig).await;
    assert_eq!(ops_named(&asked2, "revise").len(), 1);
    assert_eq!(ops_named(&asked2, "revise")[0]["draftId"], "d1");
    assert_eq!(ops_named(&asked2, "draft_report").len(), 1);
    assert!(kept(&rig).is_empty(), "off the disk once the revision is listed");
    assert!(rig.lp(0).store.revise_owed().is_empty(), "and sent");
}

fn revision() -> crate::backlog::revise::Owed {
    crate::backlog::revise::Owed {
        item: "rollout".into(),
        remaining: "Roll out every promotion".into(),
        title: "Gradual rollout".into(),
        by: Some("ana".into()),
        since: 5,
    }
}

#[tokio::test]
async fn a_report_taken_before_a_crash_that_listed_no_revision_still_revises_the_item() {
    let (base, asked, listed) = super::asks::store_failing("").await;
    let rig = rig(2, &[&base]);
    // The store took the `merged` report; the computer died before the revision was listed in the state file.
    draft_owed::open(&rig._dir.path().join("0")).put(
        "d1",
        Owed {
            lease: "lease-d".into(),
            status: "merged".into(),
            detail: Some("merged".into()),
            since: crate::owed_file::now_secs(),
            revision: Some(revision()),
            reported: true,
            ..Default::default()
        },
    );
    let mut rig = restarted(rig, &base);
    rig.go_live();
    rig.machine.drafts = Some(Arc::new(approve::Host::default()));
    pass_until(&mut rig, "revised", || !ops_named(&asked, "revise").is_empty()).await;
    quiet_passes(&mut rig).await;
    assert_eq!(ops_named(&asked, "revise").len(), 1);
    assert_eq!(
        ops_named(&asked, "revise")[0],
        json!({ "id": "rollout", "remaining": "Roll out every promotion", "draftId": "d1", "by": "ana", "title": "Gradual rollout" })
    );
    assert!(
        ops_named(&asked, "draft_report").is_empty(),
        "the report was answered: it is not sent again"
    );
    assert!(draft_owed::open(&rig._dir.path().join("0")).all().is_empty());
    assert!(listed.drafts.lock().unwrap().is_empty());
}

/// A project whose first look found the plan's pull request open, with the timed look far off and the early ones allowed at once.
async fn rig_looked_once() -> (Rig, Asked, Arc<approve::Host>, tempfile::TempDir) {
    let (mut rig, asked, _listed, host, root) = rig_landed(PrState::Open, merging(json!({ "remaining": "x" }))).await;
    *host.main.lock().unwrap() = Some("a".into());
    pass_until(&mut rig, "looked", || !host.looked.lock().unwrap().is_empty()).await;
    rig.lp(0).landing.early_gap = Duration::ZERO;
    rig.lp(0).landing.main_read_every = Duration::ZERO;
    (rig, asked, host, root)
}

#[tokio::test]
async fn a_plan_merged_by_hand_is_noticed_once_a_fetch_moves_the_default_branch() {
    let (mut rig, asked, host, _root) = rig_looked_once().await;
    // Main hasn't moved: no look, however many passes.
    quiet_passes(&mut rig).await;
    assert_eq!(host.looked.lock().unwrap().len(), 1);
    // The person merges it on the code host, and the next fetch brings main forward.
    host.states.lock().unwrap().insert(9, PrState::Merged);
    *host.main.lock().unwrap() = Some("b".into());
    pass_until(&mut rig, "merged", || !reports(&asked).is_empty()).await;
    assert_eq!(reports(&asked)[0]["status"], "merged");
}

#[tokio::test]
async fn a_plan_merged_by_hand_is_noticed_on_the_stores_drafts_changed_push() {
    let (mut rig, asked, host, _root) = rig_looked_once().await;
    host.states.lock().unwrap().insert(9, PrState::Merged);
    let push = crate::backlog_socket::Push::DraftsChanged { drafts: vec![] };
    crate::backlog_socket::wake::wake(&rig.machine.links.work, "p0", &push);
    pass_until(&mut rig, "merged", || !reports(&asked).is_empty()).await;
    assert_eq!(reports(&asked)[0]["status"], "merged");
}

#[tokio::test]
async fn a_look_asked_for_early_waits_out_the_gap_so_a_push_storm_doesnt_poll_the_code_host() {
    let (mut rig, asked, host, _root) = rig_looked_once().await;
    rig.lp(0).landing.early_gap = Duration::from_secs(3600);
    host.states.lock().unwrap().insert(9, PrState::Merged);
    *host.main.lock().unwrap() = Some("b".into());
    rig.machine.links.work.wake_for("p0", crate::work_signal::Wake::Drafts);
    quiet_passes(&mut rig).await;
    assert_eq!((host.looked.lock().unwrap().len(), reports(&asked).len()), (1, 0));
    // Still wanted: the look happens once the gap is over.
    rig.lp(0).landing.early_gap = Duration::ZERO;
    pass_until(&mut rig, "merged", || !reports(&asked).is_empty()).await;
}
