//! 2026-11-19, #4067: a run went red as `failed in 1.2m ℹ fail 0` with no comment. Every failed run names its failing
//! step in the status, posts a comment with it and the log's last lines, and sends `ci_finish` the same.
use super::*;

async fn failed_run(body: &str) -> Rig {
    let mut r = pr_rig(body);
    for t in go(&mut r, vec![key(7, "h7")]).await {
        t.await.unwrap();
    }
    r
}

#[tokio::test]
async fn a_run_that_passed_its_tests_but_failed_later_names_that_step_not_the_zero_summary() {
    let r = failed_run("echo 'ℹ fail 0'; echo '+ 03:59:01 npm run build'; echo 'tsc: not found'; exit 1").await;
    let c = calls(&r);
    assert!(c.contains("state=failure"), "{c}");
    assert!(
        c.contains("failed in") && c.contains("npm run build: tsc: not found"),
        "{c}"
    );
    assert!(!c.contains("-f description=agent-ci t: failed in 0.0m ℹ fail 0"), "{c}");
    assert_eq!(c.matches("issues/7/comments").count(), 1, "{c}");
    assert!(c.contains("at step: npm run build."), "{c}");
    let fin = &r.store.args("ci_finish")[0];
    assert_eq!(fin["result"], "failed");
    assert_eq!(fin["step"], "npm run build");
    assert!(fin["excerpt"].as_str().unwrap().contains("tsc: not found"), "{fin}");
    assert!(fin.get("reason").is_none());
}

#[tokio::test]
async fn a_log_with_nothing_in_it_still_gets_a_comment_and_an_excerpt() {
    let r = failed_run("exit 1").await;
    let c = calls(&r);
    assert_eq!(c.matches("issues/7/comments").count(), 1, "{c}");
    assert!(!r.store.args("ci_finish")[0]["excerpt"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn failed_tests_are_counted_in_the_status() {
    let r = failed_run("echo 'ℹ pass 4'; echo 'ℹ fail 3'; exit 1").await;
    assert!(calls(&r).contains("test: 3 failed"));
    assert_eq!(r.store.args("ci_finish")[0]["step"], "test");
}

#[tokio::test]
async fn a_merge_conflict_is_stated_plainly_with_the_conflict_reason() {
    let r = failed_run("echo 'error: conflicts with main; merge main into the branch'; exit 1").await;
    let c = calls(&r);
    assert!(c.contains("conflicts with main"), "{c}");
    assert!(
        c.contains("failed on h7: conflicts with main; merge main into the branch."),
        "{c}"
    );
    assert_eq!(r.store.args("ci_finish")[0]["reason"], "conflict");
}
