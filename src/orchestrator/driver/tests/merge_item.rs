//! Merge into main for an item (`driver/merge_item.rs`): a `merge_requested { itemId }` row claims the item by name,
//! squash-merges its pull request and completes it. A fake store lists the rows and answers the claim; a fake host
//! stands in for `gh` and records what the store had been told when each call came.
use super::asks::{Listed, finishes, ops_named, push, quiet_passes, store_failing, until};
use super::*;
use crate::orchestrator::driver::merge_item::MergeHost;

/// `gh`, scripted: the merge's answer and the open PR for a branch, each call noted with the store ops seen so far.
struct Gh {
    asked: Asked,
    merge: Result<(), String>,
    open: Option<u64>,
    calls: Mutex<Vec<String>>,
}

impl Gh {
    fn note(&self, what: String) {
        let ops: Vec<String> = ops_of(&self.asked);
        self.calls
            .lock()
            .unwrap()
            .push(format!("{what} after [{}]", ops.join(",")));
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn ops_of(asked: &Asked) -> Vec<String> {
    let seen = asked.lock().unwrap();
    (seen.iter().map(|(op, _)| op.clone()))
        .filter(|op| matches!(op.as_str(), "claim" | "release" | "park" | "complete"))
        .collect()
}

impl MergeHost for Gh {
    fn open_pr(&self, _: &crate::config::ProjectConfig, branch: &str) -> anyhow::Result<Option<u64>> {
        self.note(format!("pr list {branch}"));
        Ok(self.open)
    }

    fn merge(&self, _: &crate::config::ProjectConfig, pr: u64, delete: bool) -> anyhow::Result<()> {
        self.note(format!("pr merge {pr} --squash delete={delete}"));
        self.merge.clone().map_err(|e| anyhow::anyhow!(e))
    }

    fn ship(
        &self,
        _: &crate::config::ProjectConfig,
        _: &str,
        _: &[crate::backlog::Item],
    ) -> Result<Option<String>, crate::team_branch::ship::Stop> {
        unreachable!("shipping is tested against real git in team_branch/ship/tests.rs")
    }
}

/// The one item-4 park the store had, for `waits_for`, and no release: the item goes back as it was.
fn parked_for(asked: &Asked, waits_for: &str) {
    let parked = ops_named(asked, "park");
    assert_eq!(parked, [json!({ "id": "4", "token": "lease-1", "for": waits_for })]);
    assert!(ops_named(asked, "release").is_empty());
}

fn row(args: Value) -> Value {
    json!({ "id": "in-1", "kind": "merge_requested", "status": "requested", "issuedBy": "ana", "args": args })
}

fn item_row(item: &str) -> Value {
    row(json!({ "itemId": item }))
}

fn claimed(waiting: Option<&str>, item: Value) -> (u16, Value) {
    let mut value = json!({ "token": "lease-1", "item": item });
    if let Some(waiting) = waiting {
        value["wasWaiting"] = json!(waiting);
    }
    (200, json!({ "value": value }))
}

fn waiting_item() -> Value {
    json!({ "id": "4", "title": "T", "status": "claimed", "rank": 1, "slice": 2, "remaining": "the page",
        "branch": "retaskable/four", "prUrl": "https://github.com/o/r/pull/9" })
}

async fn rig_merging(
    answer: (u16, Value),
    merge: Result<(), String>,
    open: Option<u64>,
    land: &str,
) -> (Rig, Asked, Listed, Arc<Gh>) {
    let (base, asked, listed) = store_failing("").await;
    *listed.person_claim.lock().unwrap() = Some(answer);
    let gh = Arc::new(Gh {
        asked: asked.clone(),
        merge,
        open,
        calls: Default::default(),
    });
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.merge_host = Some(gh.clone());
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        project.base_branch = "main".into();
        project.github = Some(json!({ "land": land, "protectedBranch": "main" }));
    }
    lp.merging.poll_every = Duration::ZERO;
    (rig, asked, listed, gh)
}

async fn happy() -> (Rig, Asked, Listed, Arc<Gh>) {
    rig_merging(claimed(Some("merge"), waiting_item()), Ok(()), None, "person").await
}

#[tokio::test]
async fn a_merge_claims_the_item_squash_merges_its_pull_request_and_completes_it() {
    let (mut rig, asked, listed, gh) = happy().await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "done" }),
        "done with no result"
    );
    let claim = &ops_named(&asked, "claim")[0];
    assert_eq!(
        (claim["id"].as_str(), claim["because"].as_str(), claim["did"].as_str()),
        (Some("4"), Some("person"), Some("merged"))
    );
    assert_eq!(
        gh.calls(),
        ["pr merge 9 --squash delete=true after [claim]"],
        "claimed first, completed only after the merge"
    );
    let done = &ops_named(&asked, "complete")[0];
    assert_eq!(
        (done["id"].as_str(), done["token"].as_str()),
        (Some("4"), Some("lease-1"))
    );
    assert_eq!(
        (done["built"].clone(), done["remaining"].clone()),
        (json!(false), json!("the page"))
    );
    assert!(done.get("mergeCommit").is_none());
    assert!(ops_named(&asked, "release").is_empty());
    assert!(
        rig.lp(0).runs.is_empty() && rig.lp(0).leases.is_empty(),
        "nothing is kept of it here"
    );
}

#[tokio::test]
async fn a_branch_that_isnt_the_slices_own_is_not_deleted() {
    let mut item = waiting_item();
    item["branch"] = json!("feature/x");
    let (mut rig, asked, listed, gh) = rig_merging(claimed(Some("merge"), item), Ok(()), None, "person").await;
    push(&listed, item_row("4"));
    until(&mut rig, &asked, 1).await;
    assert_eq!(gh.calls(), ["pr merge 9 --squash delete=false after [claim]"]);
}

#[tokio::test]
async fn a_pr_with_no_number_in_its_url_is_found_by_its_branch() {
    let mut item = waiting_item();
    item["prUrl"] = json!("https://github.com/o/r/pulls");
    let (mut rig, asked, listed, gh) = rig_merging(claimed(Some("merge"), item), Ok(()), Some(12), "person").await;
    push(&listed, item_row("4"));
    until(&mut rig, &asked, 1).await;
    assert_eq!(
        gh.calls(),
        [
            "pr list retaskable/four after [claim]",
            "pr merge 12 --squash delete=true after [claim]"
        ]
    );
}

#[tokio::test]
async fn no_pr_number_anywhere_is_refused_and_the_claim_given_back() {
    let mut item = waiting_item();
    item["prUrl"] = json!("https://github.com/o/r/pulls");
    let (mut rig, asked, listed, gh) = rig_merging(claimed(Some("merge"), item), Ok(()), None, "person").await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(
        sent[0]["why"],
        "Lost track of this pull request: no PR number on record, and none found open for its branch."
    );
    parked_for(&asked, "merge");
    assert!(ops_named(&asked, "complete").is_empty());
    assert_eq!(gh.calls().len(), 1, "only the lookup");
}

#[tokio::test]
async fn a_merge_github_refuses_is_refused_in_its_words_and_the_claim_given_back() {
    let err = String::from("gh pr merge 9 failed: GraphQL: Pull request is not mergeable (mergePullRequest)");
    let (mut rig, asked, listed, _gh) =
        rig_merging(claimed(Some("merge"), waiting_item()), Err(err), None, "person").await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(
        sent[0]["why"],
        "Couldn't merge #9: Pull request is not mergeable (mergePullRequest)"
    );
    parked_for(&asked, "merge");
    assert!(ops_named(&asked, "complete").is_empty());
    assert!(rig.lp(0).runs.is_empty() && rig.lp(0).leases.is_empty());
}

#[tokio::test]
async fn a_long_reason_is_cut_to_300_characters() {
    let err = format!("gh pr merge 9 failed: {}", "x".repeat(500));
    let (mut rig, asked, listed, _gh) =
        rig_merging(claimed(Some("merge"), waiting_item()), Err(err), None, "person").await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    let why = sent[0]["why"].as_str().unwrap();
    assert!(why.starts_with("Couldn't merge #9: gh pr merge 9 failed: xxx"));
    assert_eq!(why.chars().count(), "Couldn't merge #9: ".len() + 300);
}

#[tokio::test]
async fn a_project_where_github_lands_the_change_refuses_without_claiming() {
    let (mut rig, asked, listed, gh) =
        rig_merging(claimed(Some("merge"), waiting_item()), Ok(()), None, "retaskable").await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It isn't waiting for you to merge it.");
    assert!(ops_named(&asked, "claim").is_empty());
    assert!(gh.calls().is_empty());
}

#[tokio::test]
async fn the_stores_refusal_of_the_claim_is_the_rows_reason_in_plain_words() {
    let refusal = (
        409,
        json!({ "error": "conflict", "message": "x",
            "why": { "reason": "held", "login": "bo", "machine": "bo-pc" } }),
    );
    let (mut rig, asked, listed, gh) = rig_merging(refusal, Ok(()), None, "person").await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(
        sent[0]["why"],
        "@bo's computer (bo-pc) is working on this right now. It'll come back to you if it needs you."
    );
    assert!(gh.calls().is_empty() && ops_named(&asked, "release").is_empty() && ops_named(&asked, "park").is_empty());
}

#[tokio::test]
async fn an_item_that_waited_for_something_else_is_parked_again_and_one_not_waiting_is_released() {
    for waiting in [None, Some("help"), Some("ci")] {
        let (mut rig, asked, listed, gh) = rig_merging(claimed(waiting, waiting_item()), Ok(()), None, "person").await;
        push(&listed, item_row("4"));
        let sent = until(&mut rig, &asked, 1).await;
        assert_eq!(sent[0]["why"], "It isn't waiting for you to merge it.", "{waiting:?}");
        match waiting {
            Some(waits_for) => parked_for(&asked, waits_for),
            None => {
                assert_eq!(ops_named(&asked, "release").len(), 1);
                assert!(ops_named(&asked, "park").is_empty());
            }
        }
        assert!(gh.calls().is_empty() && ops_named(&asked, "complete").is_empty());
    }
}

#[tokio::test]
async fn an_item_with_no_pull_request_is_given_back() {
    let mut item = waiting_item();
    item["prUrl"] = Value::Null;
    let (mut rig, asked, listed, gh) = rig_merging(claimed(Some("merge"), item), Ok(()), None, "person").await;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It isn't waiting for you to merge it.");
    parked_for(&asked, "merge");
    assert!(gh.calls().is_empty());
}

#[tokio::test]
async fn an_item_with_a_step_running_here_is_refused_with_no_claim() {
    let (mut rig, asked, listed, gh) = happy().await;
    rig.queue(0, &["4"]);
    rig.lp(0).runs.get_mut("4").unwrap().state = ItemState::Implementing;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0]["why"],
        "A step is running for this item; try again in a moment."
    );
    assert!(ops_named(&asked, "claim").is_empty() && gh.calls().is_empty());
}

#[tokio::test]
async fn an_item_stopped_here_is_not_waiting_for_a_merge() {
    let (mut rig, asked, listed, _gh) = happy().await;
    rig.queue(0, &["4"]);
    rig.lp(0).runs.get_mut("4").unwrap().state = ItemState::NeedsHuman;
    push(&listed, item_row("4"));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It isn't waiting for you to merge it.");
    assert!(ops_named(&asked, "claim").is_empty());
}

#[tokio::test]
async fn a_row_naming_more_than_one_is_refused() {
    let (mut rig, asked, listed, gh) = happy().await;
    push(&listed, row(json!({ "itemId": "4", "draftId": "draft-1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0]["why"],
        "Name one item, one draft, or the pull request into main to merge."
    );
    assert!(ops_named(&asked, "claim").is_empty() && gh.calls().is_empty());
}

#[tokio::test]
async fn a_row_is_merged_once_though_the_store_keeps_listing_it() {
    let (mut rig, asked, listed, gh) = happy().await;
    push(&listed, item_row("4"));
    until(&mut rig, &asked, 1).await;
    listed.rows.lock().unwrap()[0]["status"] = json!("running");
    quiet_passes(&mut rig).await;
    assert_eq!(gh.calls().len(), 1);
    assert_eq!(finishes(&asked).len(), 1);
}

#[tokio::test]
async fn a_row_held_from_before_a_restart_is_failed_and_never_merged() {
    let (mut rig, asked, listed, gh) = happy().await;
    let mut held = item_row("4");
    held["status"] = json!("claimed");
    push(&listed, held);
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "failed");
    assert_eq!(
        sent[0]["why"],
        "This computer restarted before it finished; check, then ask again"
    );
    assert!(ops_named(&asked, "claim").is_empty() && gh.calls().is_empty());
}

#[tokio::test]
async fn in_runs_mode_a_row_naming_an_item_is_never_claimed_and_stays_requested() {
    let (mut rig, asked, listed, gh) = happy().await;
    rig.go_runs();
    push(&listed, item_row("4"));
    push(&listed, row(json!({ "itemId": "4", "draftId": "d1" })));
    rig.runs_quiet().await;
    let kinds: Vec<Value> = ops_named(&asked, "intent_list");
    assert!(
        kinds.contains(&json!({ "kinds": ["merge_requested"] })),
        "the store was asked: {kinds:?}"
    );
    assert!(ops_named(&asked, "intent_claim").is_empty(), "no row was claimed");
    assert!(ops_named(&asked, "claim").is_empty(), "and no item");
    assert!(gh.calls().is_empty());
    let rows = listed.rows.lock().unwrap();
    assert!(rows.iter().all(|r| r["status"] == "requested"), "{rows:?}");
}
