//! The merge control into main from a `merge_requested { ship: true }` row (`driver/merge_item.rs`): the row reaches the
//! loop, which refuses a project with no team branch and hands the rest to the host. A fake host stands in for git and
//! `gh` here; `team_branch/ship/tests.rs` runs the real git and a fake `gh`.
use super::asks::{Listed, finishes, push, quiet_passes, store_failing, until};
use super::*;
use crate::backlog::Item;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::merge_item::MergeHost;
use crate::team_branch::ship::Stop;

struct Host {
    answer: Mutex<Option<Result<Option<String>, Stop>>>,
    calls: Mutex<Vec<String>>,
}

impl MergeHost for Host {
    fn open_pr(&self, _: &ProjectConfig, _: &str) -> anyhow::Result<Option<u64>> {
        unreachable!("a ship has no item")
    }
    fn merge(&self, _: &ProjectConfig, _: u64, _: bool) -> anyhow::Result<()> {
        unreachable!("a ship has no item")
    }
    fn ship(&self, p: &ProjectConfig, main: &str, items: &[Item]) -> Result<Option<String>, Stop> {
        let call = format!("ship {} {main} items={}", p.base_branch, items.len());
        self.calls.lock().unwrap().push(call);
        self.answer.lock().unwrap().take().unwrap()
    }
}

fn row(args: Value) -> Value {
    json!({ "id": "in-1", "kind": "merge_requested", "status": "requested", "issuedBy": "ana", "args": args })
}

async fn rig_shipping(answer: Result<Option<String>, Stop>, default: Option<&str>) -> (Rig, Asked, Listed, Arc<Host>) {
    let (base, asked, listed) = store_failing("").await;
    let host = Arc::new(Host {
        answer: Mutex::new(Some(answer)),
        calls: Default::default(),
    });
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.merge_host = Some(host.clone());
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        project.base_branch = "dev".into();
    }
    lp.seen.default_branch = default.map(str::to_string);
    lp.merging.poll_every = Duration::ZERO;
    (rig, asked, listed, host)
}

#[tokio::test]
async fn a_ship_ends_done_with_no_result_when_all_went_well() {
    let (mut rig, asked, listed, host) = rig_shipping(Ok(None), Some("main")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    assert_eq!(*host.calls.lock().unwrap(), ["ship dev main items=0"]);
}

#[tokio::test]
async fn a_warning_goes_into_the_rows_result() {
    let warning = "dev has new work since its pull request was read, so it wasn't started again from main.";
    let (mut rig, asked, listed, _) = rig_shipping(Ok(Some(warning.into())), Some("main")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "done", "result": { "says": warning } })
    );
}

#[tokio::test]
async fn a_refusal_ends_refused_and_a_break_ends_failed() {
    let (mut rig, asked, listed, _) = rig_shipping(Err(Stop::Refused("It conflicts.".into())), Some("main")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        (sent[0]["status"].as_str(), sent[0]["why"].as_str()),
        (Some("refused"), Some("It conflicts."))
    );

    let (mut rig, asked, listed, _) = rig_shipping(Err(Stop::Failed(anyhow::anyhow!("gh broke"))), Some("main")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "failed");
    assert!(sent[0]["why"].as_str().unwrap().contains("gh broke"));
}

#[tokio::test]
async fn a_project_on_the_default_branch_is_refused_before_the_host_is_asked() {
    let (mut rig, asked, listed, host) = rig_shipping(Ok(None), Some("dev")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(
        sent[0]["why"],
        "This project works on the repo's default branch, so there's no dev branch to send to it."
    );
    assert!(host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_row_naming_a_ship_and_an_item_is_refused_and_asks_no_one() {
    let (mut rig, asked, listed, host) = rig_shipping(Ok(None), Some("main")).await;
    push(&listed, row(json!({ "ship": true, "itemId": "4" })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0]["why"],
        "Name one item, one draft, or the pull request into main to merge."
    );
    assert!(host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_paused_project_still_ships_and_no_host_leaves_the_row_alone() {
    let (mut rig, asked, listed, _) = rig_shipping(Ok(None), Some("main")).await;
    rig.pause("p0", true);
    push(&listed, row(json!({ "ship": true })));
    assert_eq!(until(&mut rig, &asked, 1).await[0]["status"], "done");

    let (mut rig, asked, listed, _) = rig_shipping(Ok(None), Some("main")).await;
    rig.machine.merge_host = None;
    push(&listed, row(json!({ "ship": true })));
    quiet_passes(&mut rig).await;
    assert!(finishes(&asked).is_empty());
}

#[tokio::test]
async fn in_runs_mode_a_ship_is_taken_and_ends_done() {
    let (mut rig, asked, listed, host) = rig_shipping(Ok(None), Some("main")).await;
    rig.go_runs();
    push(&listed, row(json!({ "ship": true })));
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    assert_eq!(*host.calls.lock().unwrap(), ["ship dev main items=0"]);
}
