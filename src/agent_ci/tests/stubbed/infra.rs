//! One check per commit, and a runner's own trouble is no verdict on the code (2026-11-18, #797 and #798 at one head:
//! a container name conflict posted `failure` and the code agent sent the item back for nothing).
use super::*;

#[tokio::test]
async fn two_pull_requests_at_one_head_share_one_check() {
    let mut r = pr_rig("wait_gate");
    r.cfg.slots = 2;
    r.ci.poll(&r.cfg).await;
    // #797 and #798: the same head, so the same status and the same container names.
    let tasks = go(&mut r, vec![key(797, "f694e3b"), key(798, "f694e3b")]).await;
    assert_eq!(tasks.len(), 1, "the second pull request waits for the first's result");
    until("the check container", || check_runs(&r) == 1).await;
    open_gate(&r);
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(check_runs(&r), 1, "{}", calls(&r));
    assert!(!r.ci.seen.lock().unwrap().contains(&("o-p".into(), key(798, "f694e3b"))));
}

#[tokio::test]
async fn a_push_and_a_pull_request_of_one_commit_still_run_apart() {
    let mut r = pr_rig("wait_gate");
    r.cfg.slots = 2;
    r.ci.poll(&r.cfg).await;
    let push = CheckKey::push("f694e3b");
    let tasks = go(&mut r, vec![key(797, "f694e3b"), push]).await;
    assert_eq!(tasks.len(), 2, "their containers and contexts differ");
    open_gate(&r);
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn docker_refusing_to_run_is_not_a_failed_check_and_is_tried_again() {
    // `docker run` exits 125 when the daemon refuses (a name conflict, a failed pull, an unreachable daemon).
    let mut r = pr_rig("echo 'docker: Error response from daemon: Conflict.'; exit 125");
    for t in go(&mut r, vec![key(7, "h7")]).await {
        t.await.unwrap();
    }
    let c = calls(&r);
    assert!(!c.contains("state=failure") && !c.contains("state=error"), "{c}");
    assert!(!c.contains("state=success") && !c.contains("/comments"), "{c}");
    assert!(c.contains("runner trouble, trying again"), "{c}");
    assert!(r.store.args("ci_finish").is_empty(), "no result goes to the store");
    assert!(
        !r.ci.seen.lock().unwrap().contains(&("o-p".into(), key(7, "h7"))),
        "forgotten, so the next poll runs it again"
    );
    // The next poll tries again, and a working docker passes.
    std::fs::write(
        r._dir.path().join("bin/docker"),
        docker_stub(&r._dir.path().join("calls.log"), "exit 0", ""),
    )
    .unwrap();
    for t in go(&mut r, vec![key(7, "h7")]).await {
        t.await.unwrap();
    }
    assert!(calls(&r).contains("state=success"));
}

#[tokio::test]
async fn a_check_that_fails_by_its_own_exit_code_is_still_a_failure() {
    let mut r = pr_rig("exit 124");
    for t in go(&mut r, vec![key(7, "h7")]).await {
        t.await.unwrap();
    }
    assert!(calls(&r).contains("state=failure"));
}
