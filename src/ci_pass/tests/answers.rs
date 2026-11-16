//! What the pass does with answers it can't use (an unknown `do`, a refused or unreadable answer) and with its write
//! steps held (paused, signed out, at the limit).
use super::merge::{BRANCH, asks, list_and_green, pull_by};
use super::*;

#[tokio::test]
async fn an_unknown_do_runs_nothing_for_that_head_but_its_logs_and_bells_are_kept() {
    let rig = Rig::new().await;
    list_and_green(&rig, vec![pull_by(1, "feat/a", "ann", "MERGEABLE")]);
    rig.store.answers(|_| {
        json!({ "ok": true, "condition": "red", "next": { "do": "rebase" },
            "logs": [{ "msg": "kept" }], "bells": [{ "title": "t", "body": "b" }] })
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.store.sent("ci_result").len(), 1, "nothing run, so nothing said");
    assert_eq!(rig.store.sent("log_add").len(), 1);
    assert_eq!(rig.bells.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_answer_it_cant_read_skips_that_pull_request_and_the_pass_goes_on() {
    let rig = Rig::new().await;
    list_and_green(
        &rig,
        vec![
            pull_by(1, "feat/a", "ann", "MERGEABLE"),
            pull_by(2, "feat/b", "ann", "MERGEABLE"),
        ],
    );
    rig.store.answers(|a| {
        if a["pr"] == 1 {
            json!({ "ok": true, "condition": "red", "next": { "do": "rerun", "rerun": "sideways" } })
        } else {
            json!({ "ok": true, "condition": "red", "found": true, "next": null })
        }
    });
    let found = rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(found, 1, "pull request 2 was still sent and counted");
    let sent = rig.store.sent("ci_result");
    assert_eq!(
        sent.iter().map(|s| s["pr"].clone()).collect::<Vec<_>>(),
        [json!(1), json!(2)]
    );
}

#[tokio::test]
async fn a_bad_answer_to_the_did_ends_that_heads_carry_out_only() {
    let rig = Rig::new().await;
    list_and_green(
        &rig,
        vec![
            pull_by(1, "retaskable/1-a", "ann", "MERGEABLE"),
            pull_by(2, "feat/b", "ann", "MERGEABLE"),
        ],
    );
    red(rig.dir.path(), "sha1");
    rig.store.answers(|a| {
        if a["pr"] == 1 && a["did"].is_null() {
            tell(json!({ "do": "judge" }))
        } else if a["pr"] == 1 {
            json!({ "ok": "maybe" })
        } else {
            green()
        }
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.store.sent("ci_result").len(), 3);
}

#[tokio::test]
async fn paused_the_pass_still_judges_but_runs_no_fix_and_no_merge_of_main() {
    for kind in ["fix", "mergeMain"] {
        let mut rig = Rig::new().await;
        rig.hold_writes = true;
        list_and_green(&rig, vec![pull_by(9, BRANCH, "ann", "CONFLICTING")]);
        asks(&rig, kind);
        rig.pass(&mut HashMap::new(), 0).await.unwrap();
        assert_eq!(
            rig.store.sent("ci_result").len(),
            1,
            "{kind}: nothing sent after a held write"
        );
        assert!(rig.fixer.asked.lock().unwrap().is_empty(), "{kind}");
        assert!(!rig.git_log().iter().any(|l| l.contains("push")), "{kind}");
        // The head is told once: the second pass holds again without the first note being repeated.
        assert!(
            !rig.fixing.first_held_note(9, "sha9"),
            "{kind}: noted at the first pass"
        );
    }
    // A judge goes on while writes are held.
    let mut rig = Rig::new().await;
    rig.hold_writes = true;
    list_and_green(&rig, vec![pull_by(9, BRANCH, "ann", "MERGEABLE")]);
    red(rig.dir.path(), "sha9");
    asks(&rig, "judge");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.judged().len(), 1);
}

#[tokio::test]
async fn unpaused_the_fix_session_runs() {
    let rig = Rig::new().await;
    list_and_green(&rig, vec![pull_by(9, BRANCH, "ann", "MERGEABLE")]);
    asks(&rig, "fix");
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    // No repo in this rig: the session needs a worktree, so the fix says it didn't push, but it was tried.
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "fix", "pushed": false })
    );
}
