//! The session's work summary (`expert.work`): read after each turn, the pull requests read again while one is open.
use super::*;
use crate::expert::work::{Commit, FileChange, Pr, Work};

fn pr(number: u64, state: &'static str) -> Pr {
    Pr {
        number,
        title: format!("PR {number}"),
        url: format!("https://x/{number}"),
        state,
        checks: "running",
        auto_merge: false,
        branch: "b".into(),
        helper: None,
    }
}

fn files(n: &str) -> Work {
    Work {
        files: vec![FileChange {
            path: n.into(),
            added: 2,
            removed: 1,
        }],
        more: 0,
        commits: vec![Commit {
            sha: "abc1234".into(),
            subject: "Change it".into(),
            pushed: false,
        }],
        prs: vec![],
    }
}

fn set(x: &Ex, work: Option<Work>, prs: Option<Vec<Pr>>) {
    *x.host.read.lock().unwrap() = WorkRead { work, prs };
}

fn work_of(x: &Ex) -> Option<Value> {
    x.expert(ID)?.get("work").cloned()
}

#[tokio::test]
async fn the_snapshot_carries_what_the_branch_has_after_each_turn() {
    let mut x = ex().await;
    set(&x, Some(files("a.rs")), Some(vec![pr(7, "merged")]));
    x.start("s1", "Fix it");
    x.until("its work", |x| work_of(x).is_some()).await;
    let w = work_of(&x).unwrap();
    assert_eq!(w["files"][0], json!({ "path": "a.rs", "added": 2, "removed": 1 }));
    assert_eq!(w["commits"][0]["subject"], "Change it");
    assert_eq!(w["prs"][0]["number"], 7);
    assert_eq!(
        x.host.reads.lock().unwrap()[0],
        (Some(1), 0),
        "read in the session's lane"
    );

    // The next turn's read replaces it; a host that can't be read keeps the last pull requests.
    set(&x, Some(files("b.rs")), None);
    x.say("m", ID, "more", false);
    x.until("the new files", |x| {
        work_of(x).is_some_and(|w| w["files"][0]["path"] == "b.rs")
    })
    .await;
    assert_eq!(work_of(&x).unwrap()["prs"][0]["number"], 7);

    // A lane git that can't be read keeps the last files.
    set(&x, None, Some(vec![]));
    x.say("m2", ID, "again", false);
    x.until("no prs", |x| work_of(x).is_some_and(|w| w.get("prs").is_none()))
        .await;
    assert_eq!(work_of(&x).unwrap()["files"][0]["path"], "b.rs");
}

#[tokio::test]
async fn an_open_pull_request_is_read_again_without_a_turn() {
    let mut x = ex().await;
    set(&x, Some(files("a.rs")), Some(vec![pr(7, "open")]));
    x.start("s1", "Fix it");
    x.until("open", |x| work_of(x).is_some_and(|w| w["prs"][0]["state"] == "open"))
        .await;
    x.rig.lp(0).experts.beat_every = Duration::ZERO;
    set(&x, None, Some(vec![pr(7, "merged")]));
    x.until("merged", |x| {
        work_of(x).is_some_and(|w| w["prs"][0]["state"] == "merged")
    })
    .await;
    assert!(
        x.host.reads.lock().unwrap().iter().any(|(lane, _)| lane.is_none()),
        "the poll reads the pull requests alone"
    );
    let n = x.host.reads.lock().unwrap().len();
    x.quiet().await;
    assert_eq!(x.host.reads.lock().unwrap().len(), n, "nothing open: nothing read");
    assert_eq!(x.jobs().len(), 1, "no new turn");
}

#[tokio::test]
async fn nothing_is_read_for_a_pull_request_while_a_turn_runs() {
    let mut x = ex().await;
    set(&x, Some(files("a.rs")), Some(vec![pr(7, "open")]));
    x.start("s1", "Fix it");
    x.until("open", |x| work_of(x).is_some()).await;
    x.rig.lp(0).experts.beat_every = Duration::ZERO;
    x.host.hold.store(true, Ordering::SeqCst);
    x.say("m", ID, "more", false);
    x.until("running", |x| x.jobs().len() == 2).await;
    x.quiet().await;
    let polls = |x: &Ex| x.host.reads.lock().unwrap().iter().filter(|(l, _)| l.is_none()).count();
    let before = polls(&x);
    x.quiet().await;
    assert_eq!(polls(&x), before);
    x.host.hold.store(false, Ordering::SeqCst);
    x.host.go.notify_one();
}
