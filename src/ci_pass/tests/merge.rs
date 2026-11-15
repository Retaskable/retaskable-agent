//! `mayPush`, `baseHead` and `mergeMain` against a real repo with a bare origin, a fake `gh` and a git wrapper that
//! logs every call, so a force or a rebase would show.
use std::process::Command;

use super::*;
use crate::test_support::{git as tgit, repo_with_origin, run as trun, write_executable};

pub(super) const BRANCH: &str = "retaskable/9-x";

pub(super) fn commit_file(repo: &Path, name: &str, text: &str) {
    std::fs::write(repo.join(name), text).unwrap();
    trun(tgit(repo).args(["add", name]));
    trun(tgit(repo).args(["commit", "--quiet", "-m", name]));
}

pub(super) fn rev(dir: &Path, r: &str) -> String {
    let out = tgit(dir).args(["rev-parse", r]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

impl Rig {
    /// A repo with an origin that refuses a non-fast-forward, `BRANCH` on it with a change of its own and `main` moved on
    /// (one new file); git runs through a wrapper logging to `git.log`. Answers the origin's folder.
    pub(super) fn with_repo(&mut self) -> PathBuf {
        let repo = repo_with_origin(&self.dir.path().join("git"));
        let origin = repo.parent().unwrap().join("origin.git");
        trun(tgit(&origin).args(["config", "receive.denyNonFastForwards", "true"]));
        trun(tgit(&repo).args(["checkout", "--quiet", "-b", BRANCH]));
        commit_file(&repo, "feat.txt", "feat");
        trun(tgit(&repo).args(["push", "--quiet", "origin", BRANCH]));
        trun(tgit(&repo).args(["checkout", "--quiet", "main"]));
        commit_file(&repo, "m.txt", "m");
        trun(tgit(&repo).args(["push", "--quiet", "origin", "main"]));
        let wrapper = self.dir.path().join("gitlog");
        let log = self.dir.path().join("git.log");
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\n[ -n \"$1\" ] || exit 0\necho \"$*\" >> \"{}\"\nexec git \"$@\"\n",
                log.display()
            ),
        );
        self.git = Arc::new(move |dir| {
            let mut c = Command::new(&wrapper);
            c.current_dir(dir);
            c
        });
        self.repo = Some(repo);
        origin
    }

    pub(super) fn git_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("git.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn login_is(&self, who: &str) {
        std::fs::write(self.dir.path().join("login"), who).unwrap();
    }

    fn logins_read(&self) -> usize {
        self.gh_log().iter().filter(|l| l.starts_with("api user")).count()
    }
}

fn green_head(rig: &Rig, sha: &str) {
    head_is(
        rig.dir.path(),
        sha,
        vec![run_json("build", "completed", Some("success"))],
    );
}

pub(super) fn pull_by(number: u64, branch: &str, author: &str, mergeable: &str) -> Value {
    let mut p = pull(number, branch, &format!("sha{number}"));
    p["author"] = json!({ "login": author });
    p["mergeable"] = json!(mergeable);
    p
}

pub(super) fn list_and_green(rig: &Rig, pulls: Vec<Value>) {
    for p in &pulls {
        green_head(rig, p["headRefOid"].as_str().unwrap());
    }
    list(rig.dir.path(), pulls);
}

/// The store asks for `kind` (`mergeMain`, `fix`) on a head's first answer, then is green.
pub(super) fn asks(rig: &Rig, kind: &'static str) {
    rig.store.answers(move |a| {
        if a["did"].is_null() {
            tell(json!({ "do": kind }))
        } else {
            green()
        }
    });
}

fn may_push_of(sent: &[Value], pr: u64) -> bool {
    sent.iter().find(|f| f["pr"] == pr).unwrap()["mayPush"]
        .as_bool()
        .unwrap()
}

#[tokio::test]
async fn may_push_is_the_rule_with_the_signed_in_login_and_the_allow_list() {
    let mut rig = Rig::new().await;
    rig.login_is("Me\n");
    rig.allow = vec!["Bob".into()];
    list_and_green(
        &rig,
        vec![
            pull_by(1, "retaskable/1-a", "stranger", "MERGEABLE"),
            pull_by(2, "claude/b", "stranger", "MERGEABLE"),
            pull_by(3, "feat/c", "ME", "MERGEABLE"),
            pull_by(4, "feat/d", "BOB", "MERGEABLE"),
            pull_by(5, "feat/e", "stranger", "MERGEABLE"),
        ],
    );
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    let got: Vec<bool> = (1..=5).map(|n| may_push_of(&sent, n)).collect();
    assert_eq!(got, [true, true, true, true, false]);
    // The login is read once and kept.
    assert_eq!(rig.logins_read(), 1);
}

#[tokio::test]
async fn a_failed_or_empty_login_read_is_no_own_account_and_is_read_again() {
    let rig = Rig::new().await;
    list_and_green(&rig, vec![pull_by(1, "feat/a", "me", "MERGEABLE")]);
    // Signed out: `gh api user` fails.
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert!(!may_push_of(&rig.store.sent("ci_result"), 1));
    // Empty.
    rig.login_is("\n");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert!(!may_push_of(&rig.store.sent("ci_result")[1..], 1));
    assert_eq!(rig.logins_read(), 2);
    // Signed in: the next pass has it, and keeps it.
    rig.login_is("me");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert!(may_push_of(&sent[2..3], 1) && may_push_of(&sent[3..], 1));
    assert_eq!(rig.logins_read(), 3);
}

#[tokio::test]
async fn a_pull_request_with_no_author_never_reads_the_login() {
    let rig = Rig::new().await;
    let mut p = pull(1, "feat/a", "aaa");
    p["author"] = Value::Null;
    list_and_green(&rig, vec![p]);
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert!(!may_push_of(&rig.store.sent("ci_result"), 1));
    assert_eq!(rig.logins_read(), 0);
}

#[tokio::test]
async fn base_head_goes_only_with_a_conflicting_head_it_may_push_to() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    list_and_green(
        &rig,
        vec![
            pull_by(1, BRANCH, "stranger", "CONFLICTING"),
            pull_by(2, "feat/b", "stranger", "CONFLICTING"),
            pull_by(3, "retaskable/3-c", "stranger", "MERGEABLE"),
        ],
    );
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent[0]["baseHead"], rev(&origin, "main"));
    assert!(sent[1].get("baseHead").is_none(), "{}", sent[1]);
    assert!(sent[2].get("baseHead").is_none(), "{}", sent[2]);
    // The base was fetched for it, once (only the first head asked).
    assert_eq!(rig.git_log().iter().filter(|l| l.starts_with("fetch")).count(), 1);
}

#[tokio::test]
async fn merge_main_merges_and_pushes_the_branch_plainly_and_says_pushed() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    list_and_green(&rig, vec![pull_by(9, BRANCH, "stranger", "CONFLICTING")]);
    asks(&rig, "mergeMain");
    let mut since = HashMap::new();
    rig.pass(&mut since, 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1]["did"], json!({ "kind": "mergeMain", "pushed": true }));
    assert_eq!(
        sent[1]["baseHead"], sent[0]["baseHead"],
        "the second ask carries the same facts"
    );
    // A merge commit on top of the old tip, holding main's file: not a rebase, nothing rewritten.
    let tip = rev(&origin, BRANCH);
    assert_ne!(tip, before);
    assert_eq!(rev(&origin, &format!("{tip}^1")), before);
    assert_eq!(rev(&origin, &format!("{tip}^2")), rev(&origin, "main"));
    let log = rig.git_log();
    let pushes: Vec<&String> = log.iter().filter(|l| l.contains("push")).collect();
    assert_eq!(pushes.len(), 1, "{log:?}");
    assert!(
        pushes[0].ends_with(&format!("push origin HEAD:refs/heads/{BRANCH}")),
        "{pushes:?}"
    );
    // (`worktree remove --force` is the clean-up of a throwaway folder, not a push.)
    assert!(
        log.iter()
            .filter(|l| !l.contains("worktree remove"))
            .all(|l| !l.contains("--force") && !l.contains(" -f") && !l.contains("rebase") && !l.contains("+HEAD")),
        "{log:?}"
    );
    assert!(
        log.iter().any(|l| l.ends_with("merge --no-edit origin/main")),
        "{log:?}"
    );
    // The worktree is gone, and the held key with it.
    assert!(!rig.dir.path().join("worktrees/merge-ci-9").exists());
    let listed = tgit(rig.repo.as_ref().unwrap())
        .args(["worktree", "list"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("merge-ci-9"));
    assert!(since.keys().all(|k| !k.starts_with("fixing:")), "{since:?}");
}

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

#[tokio::test]
async fn a_merge_that_conflicts_is_aborted_pushes_nothing_and_says_not_pushed() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    conflict_on_c(&rig);
    let before = rev(&origin, BRANCH);
    list_and_green(&rig, vec![pull_by(9, BRANCH, "stranger", "CONFLICTING")]);
    asks(&rig, "mergeMain");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent[1]["did"], json!({ "kind": "mergeMain", "pushed": false }));
    assert_eq!(rev(&origin, BRANCH), before);
    let log = rig.git_log();
    assert!(log.iter().any(|l| l.ends_with("merge --abort")), "{log:?}");
    assert!(log.iter().all(|l| !l.contains("push")), "{log:?}");
    assert!(!rig.dir.path().join("worktrees/merge-ci-9").exists());
}

#[tokio::test]
async fn a_merge_that_fails_part_way_says_not_pushed() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    // A branch that isn't on origin (a fork's, say): there is nothing to check out.
    list_and_green(&rig, vec![pull_by(9, "retaskable/9-gone", "stranger", "CONFLICTING")]);
    asks(&rig, "mergeMain");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "mergeMain", "pushed": false })
    );
    assert!(rig.git_log().iter().all(|l| !l.contains("push")));
    assert_eq!(rev(&origin, BRANCH), before);
    assert!(!rig.dir.path().join("worktrees/merge-ci-9").exists());
}

#[tokio::test]
async fn a_head_it_may_not_push_to_is_never_merged_even_if_the_store_asks() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, "feat/x");
    list_and_green(&rig, vec![pull_by(9, "feat/x", "stranger", "CONFLICTING")]);
    asks(&rig, "mergeMain");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    // Nothing was run and no `did` sent: the carry-out ended.
    assert_eq!(rig.store.sent("ci_result").len(), 1);
    assert!(rig.git_log().is_empty(), "{:?}", rig.git_log());
    assert_eq!(before, rev(&origin, "feat/x"));
}

#[tokio::test]
async fn a_head_with_something_under_way_is_left_alone_and_its_key_kept() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    list_and_green(
        &rig,
        vec![
            pull_by(8, BRANCH, "stranger", "CONFLICTING"),
            pull_by(9, "retaskable/9-b", "stranger", "MERGEABLE"),
        ],
    );
    let mut since = HashMap::from([("fixing:8@sha8".to_string(), 1)]);
    rig.pass(&mut since, 0).await.unwrap();
    let prs: Vec<Value> = rig.store.sent("ci_result").iter().map(|f| f["pr"].clone()).collect();
    assert_eq!(prs, [json!(9)]);
    assert_eq!(since.get("fixing:8@sha8"), Some(&1));
    assert!(rig.git_log().is_empty());
    // A different head of the same pull request isn't the one under way.
    let mut since = HashMap::from([("fixing:8@old".to_string(), 1)]);
    rig.pass(&mut since, 0).await.unwrap();
    assert_eq!(rig.store.sent("ci_result").len(), 3);
}

#[tokio::test]
async fn the_held_key_is_dropped_when_the_carry_out_fails() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    list_and_green(&rig, vec![pull_by(9, BRANCH, "stranger", "CONFLICTING")]);
    // The answer to what the merge did can't be read: the pass ends with an error.
    rig.store.answers(|a| {
        if a["did"].is_null() {
            tell(json!({ "do": "mergeMain" }))
        } else {
            json!("garbled")
        }
    });
    let mut since = HashMap::new();
    assert!(rig.pass(&mut since, 0).await.is_err());
    assert!(since.keys().all(|k| !k.starts_with("fixing:")), "{since:?}");
}

fn fork(mut p: Value) -> Value {
    p["isCrossRepository"] = json!(true);
    p
}

#[tokio::test]
async fn a_fork_is_never_pushed_to_whoever_opened_it_or_whatever_its_branch_is_called() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    rig.login_is("me");
    rig.allow = vec!["ann".into()];
    let before = rev(&origin, BRANCH);
    list_and_green(
        &rig,
        vec![
            fork(pull_by(1, "feat/a", "Ann", "CONFLICTING")),
            fork(pull_by(2, "feat/b", "me", "CONFLICTING")),
            fork(pull_by(3, "claude/c", "stranger", "CONFLICTING")),
            fork(pull_by(4, BRANCH, "stranger", "CONFLICTING")),
            // The same shapes from a branch in the repository itself still push.
            pull_by(5, "feat/e", "Ann", "CONFLICTING"),
        ],
    );
    // The store asks for a merge on every head.
    asks(&rig, "mergeMain");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    for pr in 1..=4 {
        let facts = sent.iter().find(|f| f["pr"] == pr && f["did"].is_null()).unwrap();
        assert_eq!(facts["mayPush"], false, "{facts}");
        assert!(facts.get("baseHead").is_none(), "{facts}");
        assert!(
            sent.iter().filter(|f| f["pr"] == pr).count() == 1,
            "nothing was run for #{pr}"
        );
    }
    assert_eq!(rev(&origin, BRANCH), before);
    // Only #5 had a merge tried (feat/e isn't on origin, so it failed and pushed nothing).
    let five: Vec<&Value> = sent.iter().filter(|f| f["pr"] == 5).collect();
    assert_eq!(five[0]["mayPush"], true);
    assert!(five[0].get("baseHead").is_some());
    assert!(rig.git_log().iter().all(|l| !l.contains("push")));
}
