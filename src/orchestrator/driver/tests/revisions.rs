//! A plan that revises a backlog item (`driver/drafts.rs`, `driver/land.rs`, `driver/revise.rs`; docs/specs/agent/
//! 06.5-spec-drafts.md, "Revisions"): the drafter is told about the item, its `revision` is kept or dropped and
//! reported, a fix asked on the card reopens the item, and once the plan merges the item goes back to the queue with the
//! plan's remaining work, sent again until the store takes it.
use super::asks::{Listed, ops_named, push};
use super::drafts::Host;
use super::merge_draft::{person, rig_draft, row};
use super::*;

fn item() -> Value {
    json!({ "id": "rollout", "title": "Automatic rollout", "status": "todo", "slice": 1, "lease": null,
        "remaining": "Set up CI", "specRefs": ["docs/specs/rollout.md"] })
}

/// A claimed draft for the plan that revises `rollout`.
fn claimed(revises: Value) -> Value {
    json!({ "id": "d1", "request": "Roll out gradually", "titles": ["Gradual rollout"], "requestedBy": "ana", "revises": revises })
}

/// A live project whose store hands out `draft` and answers `list` with the item.
async fn rig_drafting(draft: Value, answers: Vec<Value>, fail: &'static str) -> (Rig, Arc<Host>, Asked, Listed) {
    let (base, asked, listed) = super::asks::store_failing(fail).await;
    *listed.item.lock().unwrap() = Some(item());
    listed.draft_claims.lock().unwrap().push(draft);
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    let host = Arc::new(Host::default());
    *host.answers.lock().unwrap() = answers;
    rig.machine.drafts = Some(host.clone());
    (rig, host, asked, listed)
}

/// Passes until the draft task has sent `n` `draft_report`s.
async fn reports(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.machine.pass().await;
        let sent = ops_named(asked, "draft_report");
        if sent.len() >= n {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the draft never reported {n} times");
}

fn plan(extra: Value) -> Value {
    let mut v = json!({ "status": "pr_opened", "changeTitle": "Spec: Rollout", "summary": "Revised",
        "pieces": [{ "title": "Gradual rollout", "kind": "tweak" }] });
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    v
}

#[tokio::test]
async fn the_drafter_is_told_about_the_item_its_plan_revises() {
    let (mut rig, host, asked, _) = rig_drafting(claimed(json!("rollout")), vec![plan(json!({}))], "").await;
    reports(&mut rig, &asked, 2).await;
    let told = host.revising.lock().unwrap()[0].clone().unwrap();
    assert!(told.starts_with("The item it revises:"), "{told}");
    assert!(told.contains("`rollout`, \"Automatic rollout\"") && told.contains("- What's left of it: Set up CI"));
    assert!(ops_named(&asked, "list").iter().any(|a| a["ids"] == json!(["rollout"])));
}

#[tokio::test]
async fn a_plan_that_revises_none_reads_no_item() {
    let (mut rig, host, asked, _) = rig_drafting(claimed(Value::Null), vec![plan(json!({}))], "").await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert!(host.revising.lock().unwrap()[0].is_none());
    assert!(ops_named(&asked, "list").iter().all(|a| a["ids"] != json!(["rollout"])));
    // Nothing about revisions is reported for a plan that has none.
    assert!(sent[1].get("revises").is_none() && sent[1].get("revision").is_none());
}

#[tokio::test]
async fn a_revision_for_its_item_is_reported_and_the_item_is_no_new_entry() {
    let answer = plan(
        json!({ "itemIds": ["rollout", "alerts", "alerts"], "revision": { "id": " rollout ", "remaining": " Roll out every promotion " },
        "breakdown": { "build": [{ "id": "rollout", "line": "Gradual", "size": "L" }, { "id": "alerts", "line": "Alerts", "size": "S" }], "people": [] } }),
    );
    let (mut rig, _host, asked, _) = rig_drafting(claimed(json!("rollout")), vec![answer], "").await;
    let done = reports(&mut rig, &asked, 2).await.remove(1);
    assert_eq!(done["revises"], "rollout");
    assert_eq!(done["revision"], json!({ "remaining": "Roll out every promotion" }));
    assert_eq!(done["itemIds"], json!(["alerts"]));
    // The item a revision changes has a build line of its own.
    let ids: Vec<&str> = done["breakdown"]["build"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["alerts", "rollout"]);
}

#[tokio::test]
async fn a_plan_with_no_revision_for_its_item_is_new_work_and_clears_revises() {
    for revision in [
        json!(null),
        json!({ "id": "other", "remaining": "x" }),
        json!({ "id": "rollout", "remaining": "  " }),
    ] {
        let (mut rig, _host, asked, _) = rig_drafting(
            claimed(json!("rollout")),
            vec![plan(json!({ "revision": revision }))],
            "",
        )
        .await;
        let done = reports(&mut rig, &asked, 2).await.remove(1);
        assert_eq!(
            (done["revises"].clone(), done["revision"].clone()),
            (Value::Null, Value::Null),
            "{done}"
        );
        assert!(
            done.as_object().unwrap().contains_key("revises"),
            "null is sent, so the store clears it"
        );
    }
}

#[tokio::test]
async fn a_store_that_cant_read_the_item_fails_the_plan_before_the_drafter_runs() {
    let (mut rig, host, asked, listed) = rig_drafting(claimed(json!("rollout")), vec![plan(json!({}))], "").await;
    listed.item_refused.store(true, Ordering::SeqCst);
    let sent = reports(&mut rig, &asked, 2).await;
    // A first failure goes back to the queue by itself, in words that name the item.
    assert_eq!(sent[1]["status"], "queued");
    assert!(
        sent[1]["summary"]
            .as_str()
            .unwrap()
            .contains("couldn't read item rollout"),
        "{}",
        sent[1]
    );
    assert!(host.jobs.lock().unwrap().is_empty(), "the drafter never ran");
}

#[tokio::test]
async fn a_fix_asked_on_the_card_reopens_the_item_instead_of_adding_one() {
    let answer = json!({ "status": "pr_opened", "summary": "Fix", "pieces": [{ "title": "Crash", "kind": "fix" }],
        "fixes": [{ "id": "fix-crash", "title": "Crash", "remaining": "It crashes on save" }] });
    let (mut rig, _host, asked, _) = rig_drafting(claimed(json!("rollout")), vec![answer], "").await;
    reports(&mut rig, &asked, 2).await;
    let reopened = ops_named(&asked, "reopen");
    assert_eq!(
        (
            reopened[0]["id"].as_str(),
            reopened[0]["notes"].as_str(),
            reopened[0]["by"].as_str()
        ),
        (Some("rollout"), Some("It crashes on save"), Some("ana"))
    );
    assert!(ops_named(&asked, "add").is_empty());
}

#[tokio::test]
async fn a_fix_the_store_wont_reopen_for_is_added_as_its_own_item() {
    let answer = json!({ "status": "pr_opened", "summary": "Fix", "pieces": [{ "title": "Crash", "kind": "fix" }],
        "fixes": [{ "id": "fix-crash", "title": "Crash", "remaining": "It crashes on save" }] });
    let (mut rig, _host, asked, listed) = rig_drafting(claimed(json!("rollout")), vec![answer], "").await;
    *listed.reopen.lock().unwrap() = Some(json!({ "reopened": false }));
    reports(&mut rig, &asked, 2).await;
    assert_eq!(ops_named(&asked, "add")[0]["items"][0]["id"], "fix-crash");
}

/// The store's draft: waiting for approval, revising `rollout` with what is left of it.
fn merging(revision: Value) -> Value {
    json!({ "token": "lease-d", "draft": { "id": "d1", "status": "needs_person", "detail": "awaiting_approval",
        "titles": ["Gradual rollout"], "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1-rollout",
        "itemIds": ["alerts"], "requestedBy": "ana", "revises": "rollout", "revision": revision } })
}

/// Passes until `n` `revise` calls have arrived.
async fn until_revised(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.machine.pass().await;
        let sent = ops_named(asked, "revise");
        if sent.len() >= n {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the store never got {n} revisions: {:?}", ops_named(asked, "revise"));
}

fn press() -> Value {
    row(json!({ "draftId": "d1" }))
}

#[tokio::test]
async fn a_merged_plan_puts_its_item_back_in_the_queue_with_the_plans_remaining() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, listed, _host, _root) = rig_draft(taken, person(), vec![]).await;
    push(&listed, press());
    let sent = until_revised(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "rollout", "remaining": "Roll out every promotion", "draftId": "d1", "by": "ana", "title": "Gradual rollout" })
    );
    // After the merge was told to the store, never before.
    let ops: Vec<String> = asked.lock().unwrap().iter().map(|(o, _)| o.clone()).collect();
    let at = |name: &str| ops.iter().position(|o| o == name).unwrap();
    assert!(at("draft_report") < at("revise"));
    assert!(
        rig.lp(0).landing.revisions.is_empty(),
        "taken: nothing is owed any more"
    );
    assert!(rig.lp(0).store.revise_owed().is_empty());
}

#[tokio::test]
async fn in_runs_mode_a_merged_plan_revises_its_item_too() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, listed, _host, _root) = rig_draft(taken, person(), vec![]).await;
    rig.go_runs();
    push(&listed, press());
    for _ in 0..200 {
        rig.runs_pass().await;
        if !ops_named(&asked, "revise").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(ops_named(&asked, "revise")[0]["draftId"], "d1");
}

#[tokio::test]
async fn a_merged_plan_with_no_revision_revises_nothing() {
    let mut taken = merging(json!({ "remaining": "x" }));
    taken["draft"]["revision"] = Value::Null;
    let (mut rig, asked, listed, _host, _root) = rig_draft(taken, person(), vec![]).await;
    push(&listed, press());
    super::asks::until(&mut rig, &asked, 1).await;
    for _ in 0..5 {
        rig.machine.pass().await;
    }
    assert!(ops_named(&asked, "revise").is_empty());
    assert!(rig.lp(0).landing.revisions.is_empty());
}

#[tokio::test]
async fn a_revision_the_store_didnt_take_is_kept_in_the_state_file_and_sent_again() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, listed, _host, _root) = rig_draft(taken, person(), vec![]).await;
    *listed.revise_answers.lock().unwrap() = vec![(503, json!({ "error": "busy", "message": "busy" }))];
    push(&listed, press());
    let first = until_revised(&mut rig, &asked, 1).await;
    // Listed before and after the failed send, in the saved state as well as in memory.
    assert_eq!(rig.lp(0).landing.revisions.len(), 1);
    assert_eq!(rig.lp(0).store.revise_owed()["d1"].item, "rollout");
    // The next pass sends the same call again, and the store takes it.
    let again = until_revised(&mut rig, &asked, 2).await;
    assert_eq!(first[0], again[1]);
    assert!(rig.lp(0).landing.revisions.is_empty());
    assert!(rig.lp(0).store.revise_owed().is_empty());
}

#[tokio::test]
async fn a_revision_listed_before_a_restart_is_sent_by_the_next_run() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, listed, _host, _root) = rig_draft(taken, person(), vec![]).await;
    *listed.revise_answers.lock().unwrap() = vec![(503, json!({ "error": "busy", "message": "busy" }))];
    push(&listed, press());
    until_revised(&mut rig, &asked, 1).await;
    // A new loop on the same state file starts with it listed.
    let owed = rig.lp(0).store.revise_owed();
    let landing = super::super::land::Landing::with_revisions(owed.clone());
    assert_eq!(landing.revisions, owed);
    assert_eq!(owed["d1"].remaining, "Roll out every promotion");
}

#[tokio::test]
async fn a_revision_the_store_refuses_for_good_is_dropped() {
    let taken = merging(json!({ "remaining": "Roll out every promotion" }));
    let (mut rig, asked, listed, _host, _root) = rig_draft(taken, person(), vec![]).await;
    *listed.revise_answers.lock().unwrap() = vec![(404, json!({ "error": "not_found", "message": "No such item" }))];
    push(&listed, press());
    until_revised(&mut rig, &asked, 1).await;
    for _ in 0..5 {
        rig.machine.pass().await;
    }
    assert_eq!(
        ops_named(&asked, "revise").len(),
        1,
        "a refusal that can't change isn't sent again"
    );
    assert!(rig.lp(0).landing.revisions.is_empty());
}
