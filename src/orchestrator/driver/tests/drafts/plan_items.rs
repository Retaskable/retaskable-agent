//! The items a plan adds reach the store when it merges (`driver/plan_items.rs`; Retaskable.com's backlog.md section 5, bug seen
//! 2026-11-17: plans reported `itemIds` and no item ever reached the store). No spec carries an `Items:` header: the drafter
//! returns `items`, this computer keeps them with the plan, and the store gets the ones it lacks once the plan merges, by
//! whichever path: the Approve on the lease the drafter holds, a person's Approve through `draft_take`, or a pull request a
//! person merged on the code host.
use super::super::asks::{Listed, store_failing as store_with_rows};
use super::super::merge_draft::{person, rig_draft};
use super::super::restart::restarted;
use super::*;
use crate::drafts::items::PlanItem;
use crate::orchestrator::driver::plan_items::{self, PlanItems};

fn calls(asked: &Asked, op: &str) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(o, _)| o == op)
        .map(|(_, a)| a.clone())
        .collect()
}

/// A plan with two items, the second depending on the first, one of them with a call on who checks it.
fn plan_with_items() -> Value {
    json!({ "status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Charts", "itemIds": ["not-this"],
        "pieces": [{ "title": "Charts", "kind": "feature" }],
        "items": [
            { "id": "chart-data", "title": "Chart data", "dependsOn": [], "specRefs": ["docs/specs/charts.md section 2"],
              "kind": "feature", "checking": "auto", "checkingWhy": "Internals" },
            { "id": "chart-view", "title": "Chart view", "dependsOn": ["chart-data", "elsewhere"],
              "specRefs": ["docs/specs/charts.md"], "kind": "feature" }
        ],
        "breakdown": { "build": [{ "id": "chart-view", "line": "Draw them", "size": "M" }], "people": [] } })
}

/// Passes until the store has had a call of `op`.
async fn until_called(rig: &mut Rig, asked: &Asked, op: &str) {
    for _ in 0..300 {
        rig.machine.pass().await;
        if !calls(asked, op).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the store never had a call of {op}: {:?}", ops(asked));
}

fn kept(rig: &Rig) -> std::collections::BTreeMap<String, PlanItems> {
    plan_items::open(&rig._dir.path().join("0")).all()
}

/// What the plan's items reach the store as.
fn added() -> Value {
    json!({ "via": "draft", "items": [
        { "id": "chart-data", "title": "Chart data", "dependsOn": [], "specRefs": ["docs/specs/charts.md"],
          "kind": "feature", "checking": "auto", "checkingWhy": "Internals", "requestedBy": "ada" },
        { "id": "chart-view", "title": "Chart view", "dependsOn": ["chart-data"], "specRefs": ["docs/specs/charts.md"],
          "kind": "feature", "checking": "manual", "checkingWhy": "it adds a feature", "requestedBy": "ada" }
    ] })
}

#[tokio::test]
async fn a_plan_that_approves_itself_adds_its_items_to_the_store_once_it_merges() {
    let draft = json!({ "id": "d1", "request": "Add charts", "requestedBy": "ada" });
    let (mut rig, host, asked) = rig_with(draft, vec![plan_with_items()]).await;
    until_called(&mut rig, &asked, "add").await;
    assert_eq!(*host.merges.lock().unwrap(), 1);
    assert_eq!(calls(&asked, "add"), [added()]);
    // The report carries the items' ids, not the drafter's `itemIds` (the store knows the plan's entries by them).
    let plan = (calls(&asked, "draft_report").into_iter()).find(|r| r["detail"] == "awaiting_approval");
    assert_eq!(plan.unwrap()["itemIds"], json!(["chart-data", "chart-view"]));
    // After the merge was reported, never before.
    let order: Vec<(String, Value)> = asked.lock().unwrap().clone();
    let merged = order
        .iter()
        .position(|(o, a)| o == "draft_report" && a["status"] == "merged");
    let add = order.iter().position(|(o, _)| o == "add");
    assert!(merged.is_some() && add > merged, "{merged:?} {add:?}");
    // Once the store has them nothing is kept, and nothing is sent again.
    for _ in 0..10 {
        rig.machine.pass().await;
        tokio::task::yield_now().await;
    }
    assert!(kept(&rig).is_empty());
    assert_eq!(calls(&asked, "add").len(), 1);
}

#[tokio::test]
async fn a_plan_that_waits_for_a_person_sends_nothing_until_the_approve_merges_it() {
    // Nobody asked for it: it waits for a person, and its items wait with it, kept on disk.
    let (mut rig, host, asked) =
        rig_with(json!({ "id": "d1", "request": "Add charts" }), vec![plan_with_items()]).await;
    until_called(&mut rig, &asked, "draft_release").await;
    for _ in 0..5 {
        rig.machine.pass().await;
    }
    assert!(calls(&asked, "add").is_empty());
    let listed = kept(&rig);
    assert_eq!(listed["d1"].items.len(), 2);
    assert!(!listed["d1"].merged);
    // The tier's default is on the item the drafter gave no call: a feature's plan is a person's check first.
    assert_eq!(listed["d1"].items[1].checking.as_deref(), Some("manual"));
    // A person's Approve takes the draft and merges it: now they go, with who asked (the store's word).
    rig.machine
        .links
        .item_commands
        .push_draft(crate::view::DraftCommand::Approve {
            project: "p0".into(),
            draft: "d1".into(),
        });
    until_called(&mut rig, &asked, "add").await;
    assert_eq!(*host.merges.lock().unwrap(), 1);
    assert!(!calls(&asked, "draft_take").is_empty());
    assert_eq!(calls(&asked, "add"), [added()]);
}

fn owed(merged: bool) -> PlanItems {
    let item = |id: &str, deps: &[&str]| PlanItem {
        id: id.into(),
        title: format!("Title of {id}"),
        depends_on: deps.iter().map(ToString::to_string).collect(),
        spec_refs: vec!["docs/specs/charts.md".into()],
        kind: None,
        milestone: None,
        checking: Some("manual".into()),
        checking_why: None,
    };
    PlanItems {
        items: vec![
            item("chart-data", &[]),
            item("chart-view", &["chart-data", "elsewhere"]),
        ],
        requested_by: None,
        merged,
        since: crate::owed_file::now_secs(),
    }
}

async fn rig_owing(merged: bool) -> (Rig, Asked, Listed, String) {
    let (base, asked, listed) = store_with_rows("").await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.drafts = Some(Arc::new(Host::default()));
    rig.lp(0).plan_items.put("d1", owed(merged));
    (rig, asked, listed, base)
}

#[tokio::test]
async fn a_store_that_cant_take_the_items_is_asked_again_also_after_a_restart() {
    let (mut rig, asked, listed, _) = rig_owing(true).await;
    listed.down_for.lock().unwrap().insert("add".into(), usize::MAX);
    for _ in 0..200 {
        rig.machine.pass().await;
        if calls(&asked, "add").len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(calls(&asked, "add").len() >= 3, "tried every pass");
    assert_eq!(kept(&rig).len(), 1, "still owed");
    // The computer restarts against a store that is back: the file is read and the items are added.
    let (base, asked, _listed) = store_with_rows("").await;
    let mut rig = restarted(rig, &base);
    rig.go_live();
    rig.machine.drafts = Some(Arc::new(Host::default()));
    until_called(&mut rig, &asked, "add").await;
    let sent = calls(&asked, "add");
    assert_eq!(sent[0]["via"], "draft");
    let ids: Vec<&str> = sent[0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["chart-data", "chart-view"]);
    // A dependency the store doesn't have is left off; the plan's own stays.
    assert_eq!(sent[0]["items"][1]["dependsOn"], json!(["chart-data"]));
    for _ in 0..10 {
        rig.machine.pass().await;
        tokio::task::yield_now().await;
    }
    assert!(kept(&rig).is_empty());
}

#[tokio::test]
async fn the_items_the_store_already_has_are_left_as_they_are() {
    let (mut rig, asked, listed, _) = rig_owing(true).await;
    // `list { ids }` answers the one item the store holds.
    *listed.item.lock().unwrap() = Some(json!({ "id": "chart-data", "title": "Chart data", "status": "todo" }));
    until_called(&mut rig, &asked, "add").await;
    let sent = calls(&asked, "add");
    let ids: Vec<&str> = sent[0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["chart-view"]);
    assert!(
        kept(&rig).is_empty() || {
            rig.machine.pass().await;
            kept(&rig).is_empty()
        }
    );
}

#[tokio::test]
async fn a_plan_with_no_items_sends_nothing() {
    let draft = json!({ "id": "d1", "request": "Add charts", "requestedBy": "ada" });
    let answer =
        json!({ "status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Charts", "itemIds": ["charts-1"] });
    let (mut rig, host, asked) = rig_with(draft, vec![answer]).await;
    reports(&mut rig, &asked, 3).await;
    for _ in 0..10 {
        rig.machine.pass().await;
        tokio::task::yield_now().await;
    }
    assert_eq!(*host.merges.lock().unwrap(), 1);
    assert!(calls(&asked, "add").is_empty());
    assert!(kept(&rig).is_empty());
}

#[tokio::test]
async fn a_plan_that_was_not_merged_sends_nothing_and_a_refusal_that_cant_change_is_dropped() {
    let (mut rig, asked, listed, _) = rig_owing(false).await;
    for _ in 0..10 {
        rig.machine.pass().await;
        tokio::task::yield_now().await;
    }
    assert!(calls(&asked, "add").is_empty());
    assert_eq!(kept(&rig).len(), 1);
    // Merged, and the store refuses the call as wrong (not a thing that sending again changes): dropped, once.
    rig.lp(0).plan_items.put("d1", owed(true));
    listed
        .refused
        .lock()
        .unwrap()
        .insert("add".into(), (400, "bad_request"));
    until_called(&mut rig, &asked, "add").await;
    for _ in 0..10 {
        rig.machine.pass().await;
        tokio::task::yield_now().await;
    }
    assert_eq!(calls(&asked, "add").len(), 1);
    assert!(kept(&rig).is_empty());
}

fn listed_plan() -> Value {
    json!({ "id": "d1", "status": "needs_person", "detail": "awaiting_approval", "titles": ["Charts"],
        "prUrl": "https://github.com/o/r/pull/9", "heldBy": null })
}

fn taken() -> Value {
    json!({ "token": "lease-d", "draft": { "id": "d1", "status": "needs_person", "detail": "awaiting_approval",
        "titles": ["Charts"], "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1-charts",
        "itemIds": ["chart-data", "chart-view"], "requestedBy": "ada" } })
}

#[tokio::test]
async fn a_plan_merged_on_the_code_host_adds_its_items_and_one_closed_there_drops_them() {
    use crate::drafts::land::PrState;
    for (state, adds) in [(PrState::Merged, true), (PrState::Closed, false)] {
        let (mut rig, asked, listed, host, _root) = rig_draft(taken(), person(), vec![]).await;
        listed.drafts.lock().unwrap().push(listed_plan());
        host.states.lock().unwrap().insert(9, state);
        rig.lp(0).plan_items.put("d1", owed(false));
        for _ in 0..200 {
            rig.machine.pass().await;
            if !calls(&asked, "draft_report").is_empty() && (!adds || !calls(&asked, "add").is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for _ in 0..10 {
            rig.machine.pass().await;
            tokio::task::yield_now().await;
        }
        assert_eq!(calls(&asked, "add").len(), usize::from(adds));
        assert!(kept(&rig).is_empty(), "{state:?}");
        if adds {
            // Who asked is the store's word on the plan it handed over.
            assert_eq!(calls(&asked, "add")[0]["items"][0]["requestedBy"], "ada");
        }
    }
}
