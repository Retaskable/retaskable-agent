//! Merge into main on a spec draft from a `merge_requested { draftId }` row (`driver/merge_item.rs` hands the press to
//! `driver/land.rs`): a fake store lists the row and hands out the draft, a fake host stands in for `git` and `gh`.
use super::approve::{Host, spec_repo};
use super::asks::{Listed, ops_named, push, store_failing, until};
use super::*;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::merge_item::MergeHost;

/// The item merge's `gh`, which a draft's merge must never reach.
struct NoItems;

impl MergeHost for NoItems {
    fn open_pr(&self, _: &ProjectConfig, _: &str) -> Result<Option<u64>> {
        anyhow::bail!("a draft isn't found by its branch here")
    }
    fn head(&self, _: &ProjectConfig, _: u64) -> Result<String> {
        anyhow::bail!("a draft is merged by the draft path")
    }
    fn merge(&self, _: &ProjectConfig, _: u64, _: bool, _: &str) -> Result<()> {
        anyhow::bail!("a draft is merged by the draft path")
    }

    fn ship_read(
        &self,
        _: &crate::config::ProjectConfig,
        _: &str,
        _: &[crate::backlog::Item],
    ) -> Result<crate::team_branch::ship::Shipping, crate::team_branch::ship::Stop> {
        unreachable!("shipping is tested against real git in team_branch/ship/tests.rs")
    }

    fn ship_merge(
        &self,
        _: &crate::config::ProjectConfig,
        _: &str,
        _: &crate::team_branch::ship::Shipping,
    ) -> Result<crate::team_branch::ship::Shipped, crate::team_branch::ship::Stop> {
        unreachable!("shipping is tested against real git in team_branch/ship/tests.rs")
    }
}

pub(super) fn row(args: Value) -> Value {
    json!({ "id": "in-1", "kind": "merge_requested", "status": "requested", "issuedBy": "ana", "args": args })
}

/// The draft the store hands out, in `status` and `detail`.
fn draft(status: &str, detail: &str) -> Value {
    json!({ "token": "lease-d", "draft": { "id": "d1", "status": status, "detail": detail, "titles": ["Charts"],
        "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1-charts", "itemIds": ["charts-1"] } })
}

pub(super) fn person() -> Option<Value> {
    Some(json!({ "land": "person", "protectedBranch": "main", "mergeMethod": "rebase" }))
}

/// A live project with the draft in the store, on a checkout whose origin holds its spec; `merges` are `gh pr merge`'s answers.
pub(super) async fn rig_draft(
    taken: Value,
    github: Option<Value>,
    merges: Vec<Result<(), &'static str>>,
) -> (Rig, Asked, Listed, Arc<Host>, tempfile::TempDir) {
    let (base, asked, listed) = store_failing("").await;
    *listed.draft_take.lock().unwrap() = Some(taken);
    let root = tempfile::tempdir().unwrap();
    let repo = spec_repo(root.path()).to_string_lossy().into_owned();
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        (project.repo_path, project.base_branch, project.github) = (repo.clone(), "main".into(), github.clone());
    }
    lp.seen.default_branch = Some("main".into());
    lp.merging.poll_every = Duration::ZERO;
    let host = Arc::new(Host::default());
    *host.merges.lock().unwrap() = merges;
    rig.machine.drafts = Some(host.clone());
    rig.machine.merge_host = Some(Arc::new(NoItems));
    rig.machine.merge_wait = Duration::from_millis(1);
    (rig, asked, listed, host, root)
}

fn reports(asked: &Asked) -> Vec<Value> {
    ops_named(asked, "draft_report")
}

#[tokio::test]
async fn a_draft_row_is_claimed_merged_squashed_and_ends_done_with_no_result() {
    let (mut rig, asked, listed, host, _root) =
        rig_draft(draft("needs_person", "awaiting_approval"), person(), vec![]).await;
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    assert_eq!(ops_named(&asked, "intent_claim")[0]["id"], "in-1");
    assert_eq!(ops_named(&asked, "draft_take")[0]["id"], "d1");
    // The settings say rebase; a person's merge squashes anyway, and the item path's `gh` is never asked.
    assert_eq!(host.calls(), ["merge 9 delete=true", "remove d1", "fetch"]);
    assert_eq!(*host.squashes.lock().unwrap(), [true]);
    assert_eq!(reports(&asked)[0]["status"], "merged");
    assert!(ops_named(&asked, "claim").is_empty());
}

#[tokio::test]
async fn a_draft_approved_and_waiting_for_the_merge_is_merged_too() {
    let (mut rig, asked, listed, host, _root) =
        rig_draft(draft("needs_person", "awaiting_merge"), person(), vec![]).await;
    push(&listed, row(json!({ "draftId": "d1" })));
    assert_eq!(until(&mut rig, &asked, 1).await[0]["status"], "done");
    assert_eq!(host.calls()[0], "merge 9 delete=true");
}

#[tokio::test]
async fn a_draft_that_isnt_waiting_is_refused_and_let_go_unmerged() {
    let (mut rig, asked, listed, host, _root) = rig_draft(draft("running", "drafting"), person(), vec![]).await;
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        (sent[0]["status"].as_str(), sent[0]["why"].as_str()),
        (Some("refused"), Some("Draft d1 isn't waiting for you to merge it."))
    );
    assert!(host.calls().is_empty());
    assert_eq!(ops_named(&asked, "draft_release").len(), 1);
    assert!(reports(&asked).is_empty());
}

#[tokio::test]
async fn a_project_where_a_person_doesnt_merge_into_main_refuses_the_same() {
    let landing = Some(json!({ "land": "retaskable" }));
    let (mut rig, asked, listed, host, _root) =
        rig_draft(draft("needs_person", "awaiting_approval"), landing, vec![]).await;
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "Draft d1 isn't waiting for you to merge it.");
    assert!(host.calls().is_empty());
}

#[tokio::test]
async fn a_merge_github_refuses_is_refused_in_its_words_and_noted_on_the_draft() {
    let (mut rig, asked, listed, host, _root) = rig_draft(
        draft("needs_person", "awaiting_approval"),
        person(),
        vec![Err("GraphQL: checks are failing")],
    )
    .await;
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        (sent[0]["status"].as_str(), sent[0]["why"].as_str()),
        (Some("refused"), Some("Couldn't merge #9: checks are failing"))
    );
    // One try, squashed; nothing removed.
    assert_eq!(host.calls(), ["merge 9 delete=true"]);
    assert_eq!(*host.squashes.lock().unwrap(), [true]);
    let report = &reports(&asked)[0];
    assert_eq!(report["summary"], "Couldn't merge into main: checks are failing");
    assert_eq!(report["detail"], "awaiting_approval");
}

#[tokio::test]
async fn a_draft_the_store_wont_hand_over_refuses_the_row_in_its_words() {
    let (mut rig, asked, listed, host, _root) = rig_draft(json!({ "token": 3 }), person(), vec![]).await;
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert!(sent[0]["why"].as_str().unwrap().starts_with("Couldn't take draft d1: "));
    assert!(host.calls().is_empty());
}

#[tokio::test]
async fn in_runs_mode_a_draft_row_is_merged_and_ends_done() {
    let (mut rig, asked, listed, host, _root) =
        rig_draft(draft("needs_person", "awaiting_approval"), person(), vec![]).await;
    rig.go_runs();
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    assert_eq!(host.calls(), ["merge 9 delete=true", "remove d1", "fetch"]);
    assert_eq!(reports(&asked)[0]["status"], "merged");
}

#[tokio::test]
async fn a_merged_report_the_store_cant_take_is_kept_and_sent_when_it_is_back() {
    let (mut rig, asked, listed, host, _root) =
        rig_draft(draft("needs_person", "awaiting_approval"), person(), vec![]).await;
    listed.down_for.lock().unwrap().insert("draft_report".into(), 3);
    push(&listed, row(json!({ "draftId": "d1" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "done");
    // The merge happened once; its report went 4 times: 3 refused with a 500, then taken.
    assert_eq!(host.calls(), ["merge 9 delete=true", "remove d1", "fetch"]);
    let got = reports(&asked);
    assert_eq!(got.len(), 4);
    assert!(got.iter().all(|r| r["status"] == "merged" && r["token"] == "lease-d"));
}
