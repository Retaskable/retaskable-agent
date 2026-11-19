//! `fix` against a real repo with a bare origin that refuses a non-fast-forward, a fake `gh`, a git wrapper that logs
//! every call and a fake engine that commits (or not) in its cwd.
use super::merge::{BRANCH, asks, commit_file, list_and_green, pull_by, rev};
use super::*;
use crate::test_support::{git as tgit, run as trun};

/// The worktree the fix runs in.
fn tree_of(rig: &Rig) -> PathBuf {
    rig.dir.path().join(format!("worktrees/merge-fix-9-{}", &rig.tip[..8]))
}

fn acts(rig: &Rig, f: impl Fn(&Path) -> bool + Send + Sync + 'static) {
    *rig.fixer.acts.lock().unwrap() = Box::new(f);
}

/// A fake engine that commits `fix.txt` in its worktree and goes ok.
fn commits(dir: &Path) -> bool {
    commit_file(dir, "fix.txt", "fixed");
    true
}

async fn fixes(rig: &Rig, since: &mut HashMap<String, u64>) {
    list_and_green(rig, vec![rig.at_tip(pull_by(9, BRANCH, "stranger", "MERGEABLE"))]);
    asks(rig, "fix");
    rig.pass(since, 0).await.unwrap();
}

fn pushes(rig: &Rig) -> Vec<String> {
    rig.git_log().into_iter().filter(|l| l.contains("push")).collect()
}

/// Exactly one push, `git push origin HEAD:refs/heads/<branch>` with nothing between (git's own config switches aside).
fn assert_one_plain_push(rig: &Rig) {
    let p = pushes(rig);
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(
        p[0].ends_with(&format!(" push origin HEAD:refs/heads/{BRANCH}")),
        "{p:?}"
    );
}

fn gone(rig: &Rig) {
    assert!(!tree_of(rig).exists());
    let listed = tgit(rig.repo.as_ref().unwrap())
        .args(["worktree", "list"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("merge-fix-9"));
}

fn did_of(rig: &Rig) -> Value {
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 2, "{sent:?}");
    sent[1]["did"].clone()
}

#[tokio::test]
async fn a_fix_that_commits_is_pushed_plainly_once_and_says_pushed() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    acts(&rig, commits);
    let mut since = HashMap::new();
    fixes(&rig, &mut since).await;
    assert_eq!(did_of(&rig), json!({ "kind": "fix", "pushed": true, "session": true }));
    // The fake's commit is on origin's branch, on top of the old tip.
    let tip = rev(&origin, BRANCH);
    assert_eq!(rev(&origin, &format!("{tip}^")), before);
    assert_eq!(
        String::from_utf8(
            tgit(&origin)
                .args(["show", &format!("{tip}:fix.txt")])
                .output()
                .unwrap()
                .stdout
        )
        .unwrap(),
        "fixed"
    );
    let log = rig.git_log();
    assert_one_plain_push(&rig);
    assert!(
        log.iter()
            .filter(|l| !l.contains("worktree remove"))
            .all(|l| !l.contains("--force") && !l.contains(" -f") && !l.contains("+HEAD")),
        "{log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l.contains("worktree add --detach") && l.ends_with(&format!("origin/{BRANCH}")))
    );
    // The session was asked in the worktree, with the pull request's words.
    let asked = rig.fixer.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        (asked[0].0, asked[0].1.as_str(), asked[0].2.as_str()),
        (9, "Title 9", BRANCH)
    );
    assert_eq!(asked[0].4, tree_of(&rig));
    gone(&rig);
    assert!(!rig.fixing.holds(9, &rig.tip), "the head is let go");
}

async fn nothing_pushed(rig: &mut Rig, act: impl Fn(&Path) -> bool + Send + Sync + 'static) {
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    acts(rig, act);
    fixes(rig, &mut HashMap::new()).await;
    assert_eq!(did_of(rig), json!({ "kind": "fix", "pushed": false, "session": true }));
    assert!(pushes(rig).is_empty(), "{:?}", rig.git_log());
    assert_eq!(rev(&origin, BRANCH), before);
    gone(rig);
}

#[tokio::test]
async fn a_failed_session_pushes_nothing_even_with_a_commit() {
    let mut rig = Rig::new().await;
    nothing_pushed(&mut rig, |dir| {
        commits(dir);
        false
    })
    .await;
}

#[tokio::test]
async fn a_dirty_worktree_pushes_nothing() {
    let mut rig = Rig::new().await;
    // A commit, and an untracked file left beside it.
    nothing_pushed(&mut rig, |dir| {
        commits(dir);
        std::fs::write(dir.join("left.txt"), "wip").unwrap();
        true
    })
    .await;
}

#[tokio::test]
async fn an_edit_that_was_never_committed_pushes_nothing() {
    let mut rig = Rig::new().await;
    nothing_pushed(&mut rig, |dir| {
        std::fs::write(dir.join("feat.txt"), "edited").unwrap();
        true
    })
    .await;
}

#[tokio::test]
async fn a_session_that_commits_nothing_pushes_nothing() {
    let mut rig = Rig::new().await;
    nothing_pushed(&mut rig, |_| true).await;
}

#[tokio::test]
async fn a_branch_that_moved_meanwhile_refuses_the_push_and_keeps_the_other_one() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let repo = rig.repo.clone().unwrap();
    // Someone else pushes to the branch while the session runs.
    let other = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen = other.clone();
    acts(&rig, move |dir| {
        trun(tgit(&repo).args(["checkout", "--quiet", BRANCH]));
        commit_file(&repo, "other.txt", "theirs");
        trun(tgit(&repo).args(["push", "--quiet", "origin", BRANCH]));
        trun(tgit(&repo).args(["checkout", "--quiet", "main"]));
        *seen.lock().unwrap() = rev(&repo, BRANCH);
        commits(dir)
    });
    fixes(&rig, &mut HashMap::new()).await;
    assert_eq!(did_of(&rig), json!({ "kind": "fix", "pushed": false, "session": true }));
    // The push was tried, plainly, and origin keeps the other one.
    assert_one_plain_push(&rig);
    assert_eq!(rev(&origin, BRANCH), *other.lock().unwrap());
    assert!(pushes(&rig).iter().all(|l| !l.contains("--force")));
    gone(&rig);
}

#[tokio::test]
async fn a_branch_that_is_not_on_origin_runs_no_session() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    list_and_green(
        &rig,
        vec![rig.at_tip(pull_by(9, "retaskable/9-gone", "stranger", "MERGEABLE"))],
    );
    asks(&rig, "fix");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(
        did_of(&rig),
        json!({ "kind": "fix", "pushed": false, "session": false })
    );
    assert!(rig.fixer.asked.lock().unwrap().is_empty());
    assert!(pushes(&rig).is_empty());
    assert_eq!(rev(&origin, BRANCH), before);
    gone(&rig);
}

#[tokio::test]
async fn a_head_it_may_not_push_to_is_never_fixed_even_if_the_store_asks() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    acts(&rig, commits);
    // Not its own, not allowed, not a `retaskable/` branch: and a fork's, whatever its branch is called.
    let mut fork = pull_by(8, BRANCH, "stranger", "MERGEABLE");
    fork["isCrossRepository"] = json!(true);
    list_and_green(&rig, vec![pull_by(9, "feat/x", "stranger", "MERGEABLE"), fork]);
    asks(&rig, "fix");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    // Nothing was run and no `did` sent: each carry-out ended.
    assert_eq!(rig.store.sent("ci_result").len(), 2);
    assert!(rig.fixer.asked.lock().unwrap().is_empty());
    assert!(rig.git_log().is_empty(), "{:?}", rig.git_log());
}

#[tokio::test]
async fn a_branch_that_moved_since_it_was_judged_runs_no_session_and_says_not_pushed() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let repo = rig.repo.clone().unwrap();
    let judged = rig.tip.clone();
    // The author pushes after the head was read, before the fix opens its worktree.
    trun(tgit(&repo).args(["checkout", "--quiet", BRANCH]));
    commit_file(&repo, "late.txt", "late");
    trun(tgit(&repo).args(["push", "--quiet", "origin", BRANCH]));
    trun(tgit(&repo).args(["checkout", "--quiet", "main"]));
    let moved = rev(&origin, BRANCH);
    assert_ne!(moved, judged);
    acts(&rig, commits);
    fixes(&rig, &mut HashMap::new()).await;
    assert_eq!(
        did_of(&rig),
        json!({ "kind": "fix", "pushed": false, "session": false })
    );
    assert!(
        rig.fixer.asked.lock().unwrap().is_empty(),
        "no session on code the store never saw"
    );
    assert!(pushes(&rig).is_empty());
    assert_eq!(rev(&origin, BRANCH), moved);
    gone(&rig);
}
