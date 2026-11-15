//! What the pass makes of a head's checks that are empty or can't be read.
use super::merge::pull_by;
use super::*;

#[tokio::test]
async fn a_head_with_nothing_reported_yet_is_pending_not_a_pass() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(1, "feat/a", "aaa")]);
    head_is(rig.dir.path(), "aaa", vec![]);
    let mut since = HashMap::new();
    rig.pass(&mut since, 5_000).await.unwrap();
    let sent = &rig.store.sent("ci_result")[0];
    assert_eq!(sent["checks"]["state"], "pending");
    assert_eq!(sent["pendingSince"], 5_000);
    assert_eq!(since.len(), 1, "its pending time is kept for the next pass");
}

#[tokio::test]
async fn every_pull_request_unreadable_ends_the_pass_with_an_error_saying_so() {
    let rig = Rig::new().await;
    // No status files for either head: `gh` fails, as with a token without the scope.
    list(rig.dir.path(), vec![pull(1, "feat/a", "aaa"), pull(2, "feat/b", "bbb")]);
    let err = rig.pass(&mut HashMap::new(), 0).await.unwrap_err();
    let said = format!("{err:#}");
    assert!(
        said.contains("none of the 2 open pull request(s)") && said.contains("#1"),
        "{said}"
    );
    assert!(rig.store.sent("ci_result").is_empty());
}

#[tokio::test]
async fn one_unreadable_pull_request_or_a_lone_unreadable_fork_is_no_outage() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(1, "feat/a", "aaa"), pull(2, "feat/b", "bbb")]);
    head_is(
        rig.dir.path(),
        "aaa",
        vec![run_json("build", "completed", Some("success"))],
    );
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.store.sent("ci_result").len(), 1);

    let rig = Rig::new().await;
    let mut fork = pull_by(3, "feat/c", "ann", "MERGEABLE");
    fork["isCrossRepository"] = json!(true);
    list(rig.dir.path(), vec![fork]);
    assert_eq!(rig.pass(&mut HashMap::new(), 0).await.unwrap(), 0);
}
