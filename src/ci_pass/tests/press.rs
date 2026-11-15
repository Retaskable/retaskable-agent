//! The Fix button (`press.rs`): the refusals in the TS agent's order, the facts a press sends (`button`, `mayPush`,
//! `redNow: 0`), the answer given at once while the carry-out goes on, and a fork's press, which never pushes. Real
//! repo, bare origin and fake `gh`, store, judge session and fix session as in the other pass tests. No waiting: the
//! carry-out is a task the test awaits by its handle.
use std::sync::atomic::{AtomicBool, Ordering};

use super::merge::{BRANCH, commit_file, list_and_green, pull_by, rev};
use super::*;
use crate::ci_pass::press::{NOT_WATCHING, Press, PressError};

const ALREADY: &str = "A fix is already running for pull request #9.";
const NOT_THE_ONE: &str = "This computer isn't the one watching this project's pull requests.";
const NOT_OPEN: &str = "Pull request #9 is no longer open at that commit.";
const NOT_FIXABLE: &str = "Pull request #9 isn't one the watcher can fix.";

fn refused(why: &str) -> PressError {
    PressError::Refused(why.into())
}

/// A press with its own judge and fix sessions (`Arc`, as the carry-out that runs on needs them owned).
struct Rigged {
    press: Arc<Press>,
    judge: Arc<FakeJudge>,
    fixer: Arc<FakeFixer>,
    runner: Arc<AtomicBool>,
}

impl Rig {
    fn press(&self, says: Option<(CiVerdict, &'static str)>) -> Rigged {
        let judge = Arc::new(FakeJudge {
            asked: Mutex::default(),
            says,
        });
        let fixer = Arc::new(FakeFixer {
            asked: Mutex::default(),
            acts: Mutex::new(Box::new(|_| true)),
        });
        let runner = Arc::new(AtomicBool::new(true));
        let press = Arc::new(Press {
            host: self.host(),
            judge: judge.clone(),
            fixer: fixer.clone(),
            client: Arc::new(Client::new(&self.store.url)),
            repo: 7,
            token: "tok".into(),
            items: HashSet::new(),
            allow: Vec::new(),
            logins: Arc::new(LoginCache::default()),
            bell: Arc::new(|_, _| {}),
            fixing: self.fixing.clone(),
            runner: runner.clone(),
        });
        Rigged {
            press,
            judge,
            fixer,
            runner,
        }
    }

    fn pulls_read(&self) -> usize {
        self.gh_log().iter().filter(|l| l.starts_with("pr list")).count()
    }
}

/// Judge, then the fix the store asks for on a `change` verdict, then green.
fn judge_then_fix(rig: &Rig) {
    rig.store.answers(|a| match a["did"]["kind"].as_str() {
        None => tell(json!({ "do": "judge" })),
        Some("judge") => tell(json!({ "do": "fix" })),
        Some(_) => green(),
    });
}

fn red_pull(rig: &Rig, pull: Value) {
    let sha = pull["headRefOid"].as_str().unwrap().to_string();
    list(rig.dir.path(), vec![pull]);
    red(rig.dir.path(), &sha);
}

#[tokio::test]
async fn the_refusals_come_in_the_order_the_ts_agent_checks_them() {
    let rig = Rig::new().await;
    let r = rig.press(None);
    // Nothing open: the first thing said is the running fix, whoever the computer is.
    let hold = rig.fixing.hold(9, "sha9").unwrap();
    r.runner.store(false, Ordering::SeqCst);
    assert_eq!(r.press.begin(9, "sha9").await.err(), Some(refused(ALREADY)));
    assert_eq!(rig.pulls_read(), 0);
    drop(hold);
    // Not the runner, before the pull requests are even read.
    assert_eq!(r.press.begin(9, "sha9").await.err(), Some(refused(NOT_THE_ONE)));
    assert_eq!(rig.pulls_read(), 0);
    // The runner, but the pull request isn't open, or not at that commit.
    r.runner.store(true, Ordering::SeqCst);
    list(rig.dir.path(), vec![]);
    assert_eq!(r.press.begin(9, "sha9").await.err(), Some(refused(NOT_OPEN)));
    red_pull(&rig, pull(9, "feat/a", "other"));
    assert_eq!(r.press.begin(9, "sha9").await.err(), Some(refused(NOT_OPEN)));
    assert!(rig.store.sent("ci_result").is_empty());
    // Each refusal let the head go.
    assert!(!rig.fixing.holds(9, "sha9"));
}

#[tokio::test]
async fn the_stores_refusal_is_said_in_its_words_and_has_a_fallback() {
    let rig = Rig::new().await;
    let r = rig.press(None);
    red_pull(&rig, pull(9, "feat/a", "sha9"));
    rig.store.answers(
        |_| json!({ "ok": false, "condition": "red", "refused": "Pull request #9's checks aren't failing now." }),
    );
    assert_eq!(
        r.press.begin(9, "sha9").await.err(),
        Some(refused("Pull request #9's checks aren't failing now."))
    );
    rig.store.answers(|_| json!({ "ok": false, "condition": "red" }));
    assert_eq!(r.press.begin(9, "sha9").await.err(), Some(refused(NOT_FIXABLE)));
    assert!(!rig.fixing.holds(9, "sha9"));
    // The press was sent each time.
    assert_eq!(rig.store.sent("ci_result").len(), 2);
}

#[tokio::test]
async fn a_store_or_host_that_breaks_fails_the_press_not_refuses_it() {
    let rig = Rig::new().await;
    let r = rig.press(None);
    red_pull(&rig, pull(9, "feat/a", "sha9"));
    rig.store.go_down();
    assert!(matches!(r.press.begin(9, "sha9").await, Err(PressError::Failed(_))));
    assert!(!rig.fixing.holds(9, "sha9"));
    // Checks that can't be read (no status for the head: `gh` fails): the press fails with the host's reason.
    let rig = Rig::new().await;
    let r = rig.press(None);
    list(rig.dir.path(), vec![pull(9, "feat/a", "sha9")]);
    assert!(matches!(r.press.begin(9, "sha9").await, Err(PressError::Failed(_))));
    assert!(rig.store.sent("ci_result").is_empty());
}

#[test]
fn a_computer_with_no_watcher_has_its_own_words() {
    assert_eq!(NOT_WATCHING, "This computer isn't watching pull requests.");
}

#[tokio::test]
async fn a_press_answers_at_once_and_the_fix_runs_on_with_button_facts_and_pushes_plainly() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let before = rev(&origin, BRANCH);
    // A stranger's pull request is no matter to the button.
    list_and_green(&rig, vec![pull_by(9, BRANCH, "stranger", "MERGEABLE")]);
    red(rig.dir.path(), "sha9");
    judge_then_fix(&rig);
    let r = rig.press(Some((CiVerdict::Change, "its own break")));
    *r.fixer.acts.lock().unwrap() = Box::new(|dir| {
        commit_file(dir, "fix.txt", "fixed");
        true
    });
    let started = r.press.begin(9, "sha9").await.unwrap();
    assert_eq!(started.says, "Trying a fix for pull request #9.");
    // Answered, and the carry-out has not even begun: the head is held and nothing was judged.
    assert!(rig.fixing.holds(9, "sha9"));
    assert!(r.judge.asked.lock().unwrap().is_empty());
    assert_eq!(rig.store.sent("ci_result").len(), 1);
    // A second press meanwhile.
    assert_eq!(r.press.begin(9, "sha9").await.err(), Some(refused(ALREADY)));
    started.done.await.unwrap();
    assert!(!rig.fixing.holds(9, "sha9"));
    let sent = rig.store.sent("ci_result");
    let kinds: Vec<&Value> = sent.iter().map(|f| &f["did"]["kind"]).collect();
    assert_eq!(kinds, [&Value::Null, &json!("judge"), &json!("fix")]);
    for f in &sent {
        assert_eq!(
            (&f["button"], &f["mayPush"], &f["redNow"]),
            (&json!(true), &json!(true), &json!(0))
        );
        assert_eq!(
            (&f["pr"], &f["sha"], &f["branch"]),
            (&json!(9), &json!("sha9"), &json!(BRANCH))
        );
    }
    assert_eq!(sent[2]["did"], json!({ "kind": "fix", "pushed": true }));
    assert_eq!(r.judge.asked.lock().unwrap().len(), 1);
    assert_eq!(r.fixer.asked.lock().unwrap().len(), 1);
    // The fix is on origin's branch, by one plain push.
    assert_ne!(rev(&origin, BRANCH), before);
    let pushes: Vec<String> = rig.git_log().into_iter().filter(|l| l.contains("push")).collect();
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert!(
        pushes[0].ends_with(&format!(" push origin HEAD:refs/heads/{BRANCH}")),
        "{pushes:?}"
    );
    // The button is the owner's word, so a head that was held for it can be pressed again once it ends.
    assert!(rig.fixing.hold(9, "sha9").is_some());
}

#[tokio::test]
async fn a_press_on_a_pull_request_of_a_person_not_on_the_allow_list_says_may_push() {
    let rig = Rig::new().await;
    let r = rig.press(Some((CiVerdict::NotChange, "the machine failed")));
    red_pull(&rig, pull_by(9, "feat/a", "stranger", "MERGEABLE"));
    // The head's own plain `pull` has `ann` as its author: neither the signed-in login nor on the list.
    rig.store.answers(|a| match a["did"]["kind"].as_str() {
        None => tell(json!({ "do": "judge" })),
        Some(_) => green(),
    });
    r.press.begin(9, "sha9").await.unwrap().done.await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["mayPush"], true);
    assert_eq!(sent[0]["button"], true);
    // A not-the-change verdict is the end of it: no fix, no rerun.
    assert!(r.fixer.asked.lock().unwrap().is_empty());
    assert!(rig.run_calls().is_empty());
}

#[tokio::test]
async fn a_fork_pull_request_is_refused_before_the_store_is_asked() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    let mut fork = pull_by(9, BRANCH, "ann", "CONFLICTING");
    fork["isCrossRepository"] = json!(true);
    list_and_green(&rig, vec![fork]);
    red(rig.dir.path(), "sha9");
    judge_then_fix(&rig);
    let r = rig.press(Some((CiVerdict::Change, "its own break")));
    assert_eq!(
        r.press.begin(9, "sha9").await.err(),
        Some(refused(
            "Pull request #9 comes from a fork, so this computer won't push a fix to it."
        ))
    );
    // No attempt counted, nothing run, nothing pushed, and the head let go.
    assert!(rig.store.sent("ci_result").is_empty());
    assert!(r.judge.asked.lock().unwrap().is_empty());
    assert!(r.fixer.asked.lock().unwrap().is_empty());
    assert!(rig.git_log().is_empty(), "{:?}", rig.git_log());
    assert!(!rig.fixing.holds(9, "sha9"));
}

#[tokio::test]
async fn the_rows_presser_says_what_the_press_says() {
    use crate::intents::fix_requested::{FixPress, Presser};
    let rig = Rig::new().await;
    red_pull(&rig, pull(9, "feat/a", "sha9"));
    rig.store.answers(|a| match a["did"]["kind"].as_str() {
        None => tell(json!({ "do": "judge" })),
        Some(_) => green(),
    });
    let r = rig.press(Some((CiVerdict::NotChange, "the machine failed")));
    let says = Presser(r.press.clone()).press(9, "sha9").await.unwrap();
    assert_eq!(says, "Trying a fix for pull request #9.");
    assert_eq!(
        Presser(r.press.clone()).press(9, "sha9").await,
        Err(refused(ALREADY)),
        "the fix the first press began is still held"
    );
}
