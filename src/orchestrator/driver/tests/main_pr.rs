//! Open PR to main (`driver/main_pr.rs`): a `main_pr_requested` row asks the loop, which reads the team branch and opens
//! the pull request. A fake store lists the rows; a fake host stands in for git and `gh` and notes what it was asked.
use super::asks::{Listed, finishes, push, quiet_passes, store_failing, until};
use super::*;
use crate::backlog::Item;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::main_pr::MainPrHost;
use crate::team_branch::main_pr::Draft;
use crate::team_branch::{MainPr, TeamBranch};

/// Git and `gh`, scripted: what the read finds, what `gh` says is open now, and whether `pr create` works.
struct Host {
    read: Mutex<Result<TeamBranch, String>>,
    open_now: Option<MainPr>,
    create: Result<String, String>,
    calls: Mutex<Vec<String>>,
}

impl Host {
    fn note(&self, what: &str) {
        self.calls.lock().unwrap().push(what.to_string());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl MainPrHost for Host {
    fn read(&self, p: &ProjectConfig, main: &str, items: &[Item]) -> anyhow::Result<TeamBranch> {
        self.note(&format!("read {} {main} items={}", p.base_branch, items.len()));
        self.read.lock().unwrap().clone().map_err(|e| anyhow::anyhow!(e))
    }

    fn open_pr(&self, _: &ProjectConfig, main: &str) -> anyhow::Result<Option<MainPr>> {
        self.note(&format!("open {main}"));
        Ok(self.open_now.clone())
    }

    fn draft(&self, _: &ProjectConfig, _: &str) -> anyhow::Result<Draft> {
        self.note("draft");
        Ok(Draft {
            title: "dev: 2 slices".into(),
            body: "the body".into(),
        })
    }

    fn create(&self, _: &ProjectConfig, main: &str, draft: &Draft) -> anyhow::Result<String> {
        self.note(&format!("create {main} {}", draft.title));
        self.create.clone().map_err(|e| anyhow::anyhow!(e))
    }
}

fn team_branch() -> TeamBranch {
    TeamBranch {
        base: "dev".into(),
        main: "main".into(),
        ahead: 2,
        behind: 0,
        main_pr: None,
        sync_open: false,
        merging: false,
    }
}

fn pr(number: u64) -> MainPr {
    MainPr {
        number,
        url: format!("https://github.com/o/r/pull/{number}"),
        mergeable: "MERGEABLE".into(),
        head: "abc".into(),
    }
}

fn host(read: TeamBranch) -> Host {
    Host {
        read: Mutex::new(Ok(read)),
        open_now: None,
        create: Ok("https://github.com/o/r/pull/9".into()),
        calls: Default::default(),
    }
}

fn row() -> Value {
    json!({ "id": "in-1", "kind": "main_pr_requested", "status": "requested", "issuedBy": "ana", "args": {} })
}

/// A live project on base `dev` whose default branch is `main` (or `default`), with `host` as git and `gh`.
async fn rig_opening(host: Host, default: Option<&str>) -> (Rig, Asked, Listed, Arc<Host>) {
    let (base, asked, listed) = store_failing("").await;
    let host = Arc::new(host);
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.main_pr_host = Some(host.clone());
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        project.base_branch = "dev".into();
    }
    lp.seen.default_branch = default.map(str::to_string);
    lp.opening.poll_every = Duration::ZERO;
    (rig, asked, listed, host)
}

#[tokio::test]
async fn a_request_opens_the_pull_request_and_ends_done_with_its_url() {
    let (mut rig, asked, listed, host) = rig_opening(host(team_branch()), Some("main")).await;
    push(&listed, row());
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(
        sent[0],
        json!({ "id": "in-1", "status": "done", "result": { "url": "https://github.com/o/r/pull/9" } })
    );
    assert_eq!(
        host.calls(),
        [
            "read dev main items=0",
            "open main",
            "draft",
            "create main dev: 2 slices"
        ]
    );
}

async fn refused_with(host: Host, default: Option<&str>, why: &str, calls: &[&str]) {
    let (mut rig, asked, listed, host) = rig_opening(host, default).await;
    push(&listed, row());
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused", "{sent:?}");
    assert_eq!(sent[0]["why"], why);
    assert_eq!(host.calls(), calls);
}

#[tokio::test]
async fn a_project_on_the_default_branch_is_refused_before_anything_is_read() {
    let why = "This project works on the repo's default branch, so there's no dev branch to send to it.";
    refused_with(host(team_branch()), Some("dev"), why, &[]).await;
}

#[tokio::test]
async fn an_open_pull_request_a_sync_in_flight_and_nothing_to_send_are_refused_in_order() {
    let mut open = team_branch();
    open.main_pr = Some(pr(7));
    open.behind = 1;
    open.sync_open = true;
    open.ahead = 0;
    refused_with(
        host(open),
        Some("main"),
        "PR #7 from dev into main is already open.",
        &["read dev main items=0"],
    )
    .await;
    let mut syncing = team_branch();
    syncing.behind = 2;
    syncing.sync_open = true;
    refused_with(
        host(syncing),
        Some("main"),
        "Sync main first: its sync is in flight.",
        &["read dev main items=0"],
    )
    .await;
    let mut nothing = team_branch();
    nothing.ahead = 0;
    refused_with(
        host(nothing),
        Some("main"),
        "dev has nothing main doesn't.",
        &["read dev main items=0"],
    )
    .await;
}

#[tokio::test]
async fn a_pull_request_found_open_when_gh_is_asked_again_is_refused() {
    let mut h = host(team_branch());
    h.open_now = Some(pr(8));
    refused_with(
        h,
        Some("main"),
        "PR #8 from dev into main is already open.",
        &["read dev main items=0", "open main"],
    )
    .await;
}

#[tokio::test]
async fn a_read_or_create_that_breaks_fails_the_row_in_plain_words() {
    let h = host(team_branch());
    *h.read.lock().unwrap() = Err("git fetch failed".into());
    let (mut rig, asked, listed, _) = rig_opening(h, Some("main")).await;
    push(&listed, row());
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "failed");
    assert!(
        sent[0]["why"].as_str().unwrap().contains("git fetch failed"),
        "{sent:?}"
    );

    let mut h = host(team_branch());
    h.create = Err("gh pr create failed: no".into());
    let (mut rig, asked, listed, host) = rig_opening(h, Some("main")).await;
    push(&listed, row());
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "failed");
    assert!(sent[0]["why"].as_str().unwrap().contains("gh pr create failed: no"));
    assert_eq!(host.calls().len(), 4);
}

#[tokio::test]
async fn a_paused_project_still_opens_it_and_no_host_leaves_the_row_alone() {
    let (mut rig, asked, listed, _) = rig_opening(host(team_branch()), Some("main")).await;
    rig.pause("p0", true);
    push(&listed, row());
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "done");

    let (mut rig, asked, listed, _) = rig_opening(host(team_branch()), Some("main")).await;
    rig.machine.main_pr_host = None;
    push(&listed, row());
    quiet_passes(&mut rig).await;
    assert!(finishes(&asked).is_empty());
}
