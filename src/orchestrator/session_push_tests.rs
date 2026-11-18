//! A branch the host already has (an item whose lane was freed while it waited on CI, or whose branch another lane
//! pushed to) is carried on, not started over from the base; a push the host's copy conflicts with goes back to the
//! session to merge it in instead of straight to a person (`reattachLane`, `saveLaneWork` and `PushConflictError`
//! in `agent/src/git.ts` and `steps.ts`).
#![cfg(unix)]
use super::tests::{branch, gated_outcome, gated_rig, origin_branches, runs_of};
use super::*;

const COMMIT_SMALL: &str =
    "echo 'fn a() {}' > small.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm small";

/// What it takes to put a commit named `host` on the host's copy of the branch, from a clone of its own, in a
/// shell run from `$R` (the rig's folder).
fn host_commit(branch: &str, file: &str) -> String {
    format!(
        "D=$(mktemp -d); git clone -q \"$R/origin.git\" \"$D\"; (cd \"$D\" && git checkout -q -b {branch} origin/main && echo host > {file} && git add -A && git -c user.name=h -c user.email=h@h commit -qm host && git push -q origin {branch})"
    )
}

fn host_log(rig: &super::tests::Rig, branch: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(rig.root.join("origin.git"))
        .args(["log", "--format=%s", branch])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn a_push_the_host_copy_conflicts_with_goes_back_to_the_session_to_merge_it_in() {
    let b = branch();
    let lane = format!(
        "{}; echo lane > f.txt; git add -A; git -c user.name=t -c user.email=t@t commit -qm lane",
        host_commit(&b, "f.txt")
    );
    let merge = format!("git -c user.name=t -c user.email=t@t merge -q -s ours --no-edit origin/{b}");
    // The first run commits `f.txt` against the host's own; the second, resumed with the finding, merges it in.
    let fetch_merge = format!("git fetch -q origin; {merge}");
    let rig = gated_rig(&[&lane, &fetch_merge]);

    let got = gated_outcome(&rig).await;

    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert_eq!(runs_of(&rig), 2);
    let prompt = std::fs::read_to_string(rig.root.join("prompt2.txt")).unwrap();
    assert!(prompt.contains(&format!("git merge origin/{b}")), "{prompt}");
    assert!(prompt.contains("don't rebase"), "{prompt}");
    let log = host_log(&rig, &b);
    assert!(
        log.contains("host") && log.contains("lane"),
        "never forced, so both are there: {log}"
    );
    assert!(
        got.notes.iter().any(|n| n.contains("its remote copy conflicts")),
        "{:?}",
        got.notes
    );
    assert!(got.notes.last().unwrap().starts_with("Opened PR #7"), "{:?}", got.notes);
}

#[tokio::test]
async fn a_conflict_that_is_never_settled_needs_a_person_after_the_send_backs() {
    let b = branch();
    let lane = format!(
        "{}; echo lane > f.txt; git add -A; git -c user.name=t -c user.email=t@t commit -qm lane",
        host_commit(&b, "f.txt")
    );
    // Every later run changes nothing, so the push conflicts each time.
    let steps: Vec<&str> = std::iter::once(lane.as_str())
        .chain(std::iter::repeat_n("true", MAX_PUSH_SEND_BACKS as usize))
        .collect();
    let rig = gated_rig(&steps);

    let got = gated_outcome(&rig).await;

    match got.result {
        SessionResult::NeedsHuman(why) => assert!(why.contains("couldn't be published"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(runs_of(&rig), 1 + MAX_PUSH_SEND_BACKS as usize);
}

#[tokio::test]
async fn a_lane_started_for_an_item_whose_branch_the_host_has_carries_that_work_on() {
    let b = branch();
    let rig = gated_rig(&[&format!("test -f host.txt || exit 9; {COMMIT_SMALL}")]);
    let setup = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("R={}; {}", rig.root.display(), host_commit(&b, "host.txt")))
        .output()
        .unwrap();
    assert!(setup.status.success(), "{}", String::from_utf8_lossy(&setup.stderr));
    assert!(origin_branches(&rig).contains(&b));

    let got = gated_outcome(&rig).await;

    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    let log = host_log(&rig, &b);
    assert!(log.contains("host") && log.contains("small"), "{log}");
}
