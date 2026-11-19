//! A conflicting merge of main is handed to a fix session (a merge, never a rebase), pushed plainly when it commits,
//! counted by the store as the attempt either way; and a red goes judge, then fix.
use super::merge::{BRANCH, asks, commit_file, list_and_green, pull_by, rev};
use super::*;
use crate::ci_fix::FixTask;
use crate::test_support::{git as tgit, run as trun};

/// `BRANCH` and `main` both changed `c.txt`.
fn conflict_on_c(rig: &Rig) {
    let repo = rig.repo.as_ref().unwrap();
    trun(tgit(repo).args(["checkout", "--quiet", BRANCH]));
    commit_file(repo, "c.txt", "theirs");
    trun(tgit(repo).args(["push", "--quiet", "origin", BRANCH]));
    trun(tgit(repo).args(["checkout", "--quiet", "main"]));
    commit_file(repo, "c.txt", "ours");
    trun(tgit(repo).args(["push", "--quiet", "origin", "main"]));
}

/// A fake engine that does what the instruction says: merges origin/main, resolves `c.txt` keeping both, commits.
fn merges_both(dir: &Path) -> bool {
    let _ = tgit(dir).args(["merge", "--no-edit", "origin/main"]).output();
    std::fs::write(dir.join("c.txt"), "ours and theirs").unwrap();
    trun(tgit(dir).args(["add", "c.txt"]));
    trun(tgit(dir).args(["commit", "--quiet", "--no-edit"]));
    true
}

async fn conflicting(rig: &mut Rig) -> PathBuf {
    let origin = rig.with_repo();
    conflict_on_c(rig);
    rig.tip = rev(&origin, BRANCH);
    list_and_green(rig, vec![rig.at_tip(pull_by(9, BRANCH, "stranger", "CONFLICTING"))]);
    asks(rig, "mergeMain");
    origin
}

#[tokio::test]
async fn a_conflicting_merge_starts_a_fix_session_with_the_conflict_instruction_and_pushes_the_merge() {
    let mut rig = Rig::new().await;
    let origin = conflicting(&mut rig).await;
    let before = rev(&origin, BRANCH);
    *rig.fixer.acts.lock().unwrap() = Box::new(merges_both);
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let asked = rig.fixer.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].3,
        FixTask::Merge(crate::merge_update::conflict_instruction(
            "main",
            &["c.txt".to_string()]
        ))
    );
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "mergeMain", "pushed": true })
    );
    // One plain merge commit on top of the old tip, holding both sides; never a rebase or a force.
    let tip = rev(&origin, BRANCH);
    assert_ne!(tip, before);
    assert_eq!(rev(&origin, &format!("{tip}^1")), before);
    assert_eq!(rev(&origin, &format!("{tip}^2")), rev(&origin, "main"));
    let log = rig.git_log();
    let pushes: Vec<_> = log.iter().filter(|l| l.contains("push")).collect();
    assert_eq!(pushes.len(), 1, "{log:?}");
    assert!(pushes[0].ends_with(&format!(" push origin HEAD:refs/heads/{BRANCH}")));
    assert!(
        log.iter()
            .filter(|l| !l.contains("worktree remove"))
            .all(|l| !l.contains("rebase") && !l.contains("--force") && !l.contains("+HEAD")),
        "{log:?}"
    );
    assert!(!rig.fixing.holds(9, &rig.tip), "the head is let go");
}

#[tokio::test]
async fn a_conflict_session_that_fails_counts_an_attempt_and_pushes_nothing() {
    let mut rig = Rig::new().await;
    let origin = conflicting(&mut rig).await;
    let before = rev(&origin, BRANCH);
    *rig.fixer.acts.lock().unwrap() = Box::new(|dir| {
        merges_both(dir);
        false
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.fixer.asked.lock().unwrap().len(), 1);
    // The store is told, so it counts the attempt against its cap.
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "mergeMain", "pushed": false })
    );
    assert_eq!(rev(&origin, BRANCH), before);
    assert!(rig.git_log().iter().all(|l| !l.contains("push")));
}

#[tokio::test]
async fn a_conflict_session_that_leaves_the_merge_unfinished_pushes_nothing() {
    let mut rig = Rig::new().await;
    let origin = conflicting(&mut rig).await;
    let before = rev(&origin, BRANCH);
    *rig.fixer.acts.lock().unwrap() = Box::new(|dir| {
        let _ = tgit(dir).args(["merge", "--no-edit", "origin/main"]).output();
        true
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "mergeMain", "pushed": false })
    );
    assert_eq!(rev(&origin, BRANCH), before);
}

#[tokio::test]
async fn a_clean_merge_runs_no_session() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    list_and_green(&rig, vec![pull_by(9, BRANCH, "stranger", "CONFLICTING")]);
    asks(&rig, "mergeMain");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert!(rig.fixer.asked.lock().unwrap().is_empty());
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "mergeMain", "pushed": true })
    );
}

#[tokio::test]
async fn a_red_goes_from_the_judge_to_the_fix_and_the_fix_is_pushed() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    rig.judge.says = Some((CiVerdict::Change, "its own test broke"));
    *rig.fixer.acts.lock().unwrap() = Box::new(|dir| {
        commit_file(dir, "fix.txt", "fixed");
        true
    });
    let p = rig.at_tip(pull_by(9, BRANCH, "stranger", "MERGEABLE"));
    red(rig.dir.path(), p["headRefOid"].as_str().unwrap());
    list(rig.dir.path(), vec![p]);
    // The store answers what a red asks, one step per `did`.
    rig.store.answers(|a| match a["did"]["kind"].as_str() {
        None => tell(json!({ "do": "judge" })),
        Some("judge") if a["did"]["verdict"] == "change" => tell(json!({ "do": "fix" })),
        _ => green(),
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.judged().len(), 1);
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(
        sent[1]["did"],
        json!({ "kind": "judge", "verdict": "change", "report": false })
    );
    assert_eq!(sent[2]["did"], json!({ "kind": "fix", "pushed": true }));
    let asked = rig.fixer.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert!(matches!(asked[0].3, FixTask::Red(_)));
    assert_ne!(rev(&origin, BRANCH), before);
}
