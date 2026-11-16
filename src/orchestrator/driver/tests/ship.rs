//! The merge control into main from a `merge_requested { ship: true }` row (`driver/merge_item.rs`): the row reaches the
//! loop, which refuses a project with no team branch and hands the rest to the host. A fake host stands in for git and
//! `gh` here; `team_branch/ship/tests.rs` runs the real git and a fake `gh`.
use super::asks::{Listed, finishes, ops_named, push, quiet_passes, store_failing, until};
use super::*;
use crate::backlog::Item;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::merge_item::MergeHost;
use crate::team_branch::ship::{Shipped, Shipping, Stop};

const HEAD: &str = "dev-head";
const COMMIT: &str = "squash-commit";

/// The host, scripted: what each read and the merge answer. Each call is noted in the store's own log, so the order of
/// the store's ops and the host's calls reads as one.
struct Host {
    reads: Mutex<std::collections::VecDeque<Result<Shipping, Stop>>>,
    answer: Mutex<Option<Result<Shipped, Stop>>>,
    asked: Asked,
    calls: Mutex<Vec<String>>,
}

impl Host {
    fn note(&self, what: &str, args: Value) {
        self.asked.lock().unwrap().push((what.to_string(), args));
    }
}

impl MergeHost for Host {
    fn open_pr(&self, _: &ProjectConfig, _: &str) -> anyhow::Result<Option<u64>> {
        unreachable!("a ship has no item")
    }
    fn head(&self, _: &ProjectConfig, _: u64) -> anyhow::Result<String> {
        unreachable!("a ship has no item")
    }
    fn merge(&self, _: &ProjectConfig, _: u64, _: bool, _: &str) -> anyhow::Result<()> {
        unreachable!("a ship has no item")
    }
    fn ship_read(&self, p: &ProjectConfig, main: &str, items: &[Item]) -> Result<Shipping, Stop> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("read {} {main} items={}", p.base_branch, items.len()));
        self.note("ship_read", Value::Null);
        self.reads.lock().unwrap().pop_front().unwrap_or_else(|| Ok(shipping()))
    }
    fn ship_merge(&self, p: &ProjectConfig, main: &str, shipping: &Shipping) -> Result<Shipped, Stop> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("merge {} {main}", p.base_branch));
        self.note(
            "gh pr merge",
            json!({ "number": shipping.number, "head": shipping.head }),
        );
        self.answer.lock().unwrap().take().unwrap()
    }
}

fn shipping() -> Shipping {
    Shipping {
        number: 7,
        url: "https://github.com/o/r/pull/7".into(),
        head: HEAD.into(),
    }
}

fn shipped(warning: Option<&str>) -> Result<Shipped, Stop> {
    Ok(Shipped {
        merged: true,
        merge_commit: Some(COMMIT.into()),
        warning: warning.map(str::to_string),
    })
}

fn row(args: Value) -> Value {
    json!({ "id": "in-1", "kind": "merge_requested", "status": "requested", "issuedBy": "ana", "args": args })
}

async fn rig_shipping(answer: Result<Shipped, Stop>, default: Option<&str>) -> (Rig, Asked, Listed, Arc<Host>) {
    let (base, asked, listed) = store_failing("").await;
    let host = Arc::new(Host {
        reads: Default::default(),
        answer: Mutex::new(Some(answer)),
        asked: asked.clone(),
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
    let (mut rig, asked, listed, host) = rig_shipping(shipped(None), Some("main")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["read dev main items=0", "read dev main items=0", "merge dev main"]
    );
}

#[tokio::test]
async fn a_warning_goes_into_the_rows_result() {
    let warning = "dev has new work since its pull request was read, so it wasn't started again from main.";
    let (mut rig, asked, listed, _) = rig_shipping(shipped(Some(warning)), Some("main")).await;
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
    let (mut rig, asked, listed, host) = rig_shipping(shipped(None), Some("dev")).await;
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
    let (mut rig, asked, listed, host) = rig_shipping(shipped(None), Some("main")).await;
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
    let (mut rig, asked, listed, _) = rig_shipping(shipped(None), Some("main")).await;
    rig.pause("p0", true);
    push(&listed, row(json!({ "ship": true })));
    assert_eq!(until(&mut rig, &asked, 1).await[0]["status"], "done");

    let (mut rig, asked, listed, _) = rig_shipping(shipped(None), Some("main")).await;
    rig.machine.merge_host = None;
    push(&listed, row(json!({ "ship": true })));
    quiet_passes(&mut rig).await;
    assert!(finishes(&asked).is_empty());
}

#[tokio::test]
async fn in_runs_mode_a_ship_is_taken_and_ends_done() {
    let (mut rig, asked, listed, host) = rig_shipping(shipped(None), Some("main")).await;
    rig.go_runs();
    push(&listed, row(json!({ "ship": true })));
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    assert_eq!(
        *host.calls.lock().unwrap(),
        ["read dev main items=0", "read dev main items=0", "merge dev main"]
    );
}

#[tokio::test]
async fn a_sync_another_computer_holds_the_lock_for_refuses_the_ship_before_the_host_is_asked() {
    let (mut rig, asked, listed, host) = rig_shipping(shipped(None), Some("main")).await;
    *listed.lock_held_by.lock().unwrap() = Some("other-pc".into());
    rig.go_runs();
    push(&listed, row(json!({ "ship": true })));
    let sent = rig.runs_until(&asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    assert_eq!(sent[0]["why"], "Wait for main to finish coming into dev first.");
    assert_eq!(ops_named(&asked, "lock_take")[0]["name"], "sync_main-dev");
    assert!(host.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_ship_holds_the_sync_lock_while_it_runs_and_gives_it_back_after_either_answer() {
    for answer in [shipped(None), Err(Stop::Refused("It conflicts.".into()))] {
        let (mut rig, asked, listed, _) = rig_shipping(answer, Some("main")).await;
        push(&listed, row(json!({ "ship": true })));
        until(&mut rig, &asked, 1).await;
        assert_eq!(ops_named(&asked, "lock_take").len(), 1);
        assert_eq!(ops_named(&asked, "lock_drop"), [json!({ "token": "lock-1" })]);
        assert_eq!(rig.lp(0).run_loop.runs().locks().count(), 0, "and no longer beaten");
    }
}

/// The store's merge ops and the host's calls in the order they came, a beat or a result with its phase or outcome.
fn seq(asked: &Asked) -> Vec<String> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op.starts_with("merge_") || op.starts_with("ship_") || op == "gh pr merge")
        .map(|(op, args)| match op.as_str() {
            "merge_beat" => format!("beat:{}", args["phase"].as_str().unwrap()),
            "merge_result" => format!("result:{}", args["outcome"].as_str().unwrap()),
            _ => op.clone(),
        })
        .collect()
}

#[tokio::test]
async fn a_ship_takes_the_dev_branchs_merge_turn_beats_it_and_gives_it_back_merged() {
    let (mut rig, asked, listed, _) = rig_shipping(shipped(None), Some("main")).await;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0], json!({ "id": "in-1", "status": "done" }));
    let ours = seq(&asked);
    // The first withdraw is the sweep after a start; the ship's own follows it.
    assert_eq!(
        ours,
        [
            "merge_withdraw",
            "merge_withdraw",
            "ship_read",
            "merge_enqueue",
            "merge_next",
            "beat:updating",
            "ship_read",
            "beat:merging",
            "gh pr merge",
            "result:merged",
            "merge_withdraw",
        ]
    );
    assert_eq!(
        ops_named(&asked, "merge_withdraw"),
        vec![json!({ "item": "ship-dev" }); 3]
    );
    assert_eq!(
        ops_named(&asked, "merge_enqueue"),
        [
            json!({ "item": "ship-dev", "base": "dev", "head": HEAD, "land": "retaskable",
            "pr": { "number": 7, "url": "https://github.com/o/r/pull/7" }, "chore": true, "holdsBranch": true })
        ]
    );
    // Without `piece: true`, which would make the store answer `waiting`.
    assert_eq!(ops_named(&asked, "merge_next"), [json!({ "base": "dev" })]);
    assert_eq!(
        ops_named(&asked, "merge_result"),
        [json!({ "token": "turn-1", "outcome": "merged", "mergeCommit": COMMIT })]
    );
    assert_eq!(ops_named(&asked, "gh pr merge"), [json!({ "number": 7, "head": HEAD })]);
}

#[tokio::test]
async fn a_turn_that_is_not_free_refuses_in_words_withdraws_and_never_merges() {
    let cases = [
        (
            json!({ "waiting": { "place": 1, "ahead": "item-9" } }),
            "A piece is being merged into dev (waiting on item-9). Try again when it has landed.",
        ),
        (
            json!({ "waiting": { "place": 2, "ahead": null } }),
            "A piece is being merged into dev. Try again when it has landed.",
        ),
        (Value::Null, "The project is paused, so the merge turn can't be taken."),
    ];
    for (answer, why) in cases {
        let (mut rig, asked, listed, _) = rig_shipping(shipped(None), Some("main")).await;
        *listed.merge_next.lock().unwrap() = Some(answer);
        push(&listed, row(json!({ "ship": true })));
        let sent = until(&mut rig, &asked, 1).await;
        assert_eq!(
            (sent[0]["status"].as_str(), sent[0]["why"].as_str()),
            (Some("refused"), Some(why))
        );
        let ours = seq(&asked);
        assert!(!ours.contains(&"gh pr merge".to_string()), "{ours:?}");
        assert_eq!(&ours[ours.len() - 2..], ["merge_next", "merge_withdraw"], "{ours:?}");
        assert!(ops_named(&asked, "merge_result").is_empty(), "no turn was held");
        assert_eq!(rig.lp(0).merging.ship_turn, None);
    }
}

#[tokio::test]
async fn every_ship_that_took_the_turn_but_did_not_merge_ends_closed_and_withdrawn() {
    let queued = Ok(Shipped {
        merged: false,
        merge_commit: None,
        warning: Some("PR #7 didn't go in straight away (GitHub may have queued it), so dev was left as it is.".into()),
    });
    let no_commit = Ok(Shipped {
        merged: true,
        merge_commit: None,
        warning: None,
    });
    for answer in [
        Err(Stop::Refused("GitHub didn't merge PR #7: nope".into())),
        Err(Stop::Failed(anyhow::anyhow!("gh broke"))),
        queued,
        no_commit,
    ] {
        let (mut rig, asked, listed, _) = rig_shipping(answer, Some("main")).await;
        push(&listed, row(json!({ "ship": true })));
        until(&mut rig, &asked, 1).await;
        let ours = seq(&asked);
        assert_eq!(
            &ours[ours.len() - 3..],
            ["gh pr merge", "result:closed", "merge_withdraw"],
            "{ours:?}"
        );
        assert!(!ours.iter().any(|o| o == "result:released"));
        assert_eq!(rig.lp(0).merging.ship_turn, None, "the held turn is forgotten");
    }
}

#[tokio::test]
async fn a_read_that_refuses_with_the_turn_held_ends_closed_and_before_the_turn_nothing_is_queued() {
    // The second read (turn held) refuses: closed, withdrawn, no merge.
    let (mut rig, asked, listed, host) = rig_shipping(shipped(None), Some("main")).await;
    host.reads
        .lock()
        .unwrap()
        .extend([Ok(shipping()), Err(Stop::Refused("It conflicts.".into()))]);
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["why"], "It conflicts.");
    let ours = seq(&asked);
    assert_eq!(
        &ours[ours.len() - 4..],
        ["beat:updating", "ship_read", "result:closed", "merge_withdraw"]
    );

    // The first read (no turn yet) refuses: no entry is queued and no turn asked for.
    let (mut rig2, asked2, listed2, host2) = rig_shipping(shipped(None), Some("main")).await;
    host2
        .reads
        .lock()
        .unwrap()
        .push_back(Err(Stop::Refused("No pull request from dev into main is open.".into())));
    push(&listed2, row(json!({ "ship": true })));
    until(&mut rig2, &asked2, 1).await;
    assert!(ops_named(&asked2, "merge_enqueue").is_empty() && ops_named(&asked2, "merge_next").is_empty());
    drop((rig, rig2));
}

#[tokio::test]
async fn a_turn_the_store_says_is_lost_before_the_merge_stops_the_ship() {
    for phase in ["updating", "merging"] {
        let (mut rig, asked, listed, _) = rig_shipping(shipped(None), Some("main")).await;
        *listed.beat_lost_at.lock().unwrap() = Some(phase.into());
        push(&listed, row(json!({ "ship": true })));
        let sent = until(&mut rig, &asked, 1).await;
        assert_eq!(sent[0]["why"], "The merge turn was lost, so nothing was merged.");
        let ours = seq(&asked);
        assert!(!ours.contains(&"gh pr merge".to_string()), "{ours:?}");
        assert_eq!(&ours[ours.len() - 2..], ["result:closed", "merge_withdraw"]);
    }
}

#[tokio::test]
async fn a_store_that_cannot_be_reached_fails_the_ship_closed() {
    let (base, asked, listed) = store_failing("merge_enqueue").await;
    let host = Arc::new(Host {
        reads: Default::default(),
        answer: Mutex::new(Some(shipped(None))),
        asked: asked.clone(),
        calls: Default::default(),
    });
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.merge_host = Some(host);
    let lp = rig.lp(0);
    for project in [&mut lp.project, &mut lp.file_project] {
        project.base_branch = "dev".into();
    }
    lp.seen.default_branch = Some("main".into());
    lp.merging.poll_every = Duration::ZERO;
    push(&listed, row(json!({ "ship": true })));
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "failed");
    let ours = seq(&asked);
    assert!(!ours.contains(&"gh pr merge".to_string()) && !ours.contains(&"merge_next".to_string()));
    assert_eq!(ours.last().map(String::as_str), Some("merge_withdraw"));
}

#[tokio::test]
async fn a_start_withdraws_a_ships_leftover_once() {
    let (mut rig, asked, _, _) = rig_shipping(shipped(None), Some("main")).await;
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(ops_named(&asked, "merge_withdraw"), [json!({ "item": "ship-dev" })]);
}
