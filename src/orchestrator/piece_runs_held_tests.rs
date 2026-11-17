//! The real diff's facts a `build` run sends once its review approved (pools-on-server.md section 18.4): `result.diff`, for the store to
//! decide from whether a person checks the change first.
use super::*;

fn commit(files: &str) -> String {
    format!("{files}; git add -A; git -c user.name=t -c user.email=t@t commit -qm change")
}

const MIGRATION: &str = "mkdir -p db/migrations; echo 'DROP TABLE t;' > db/migrations/1.sql";

async fn build(files: &str, extra: Value) -> RunEnd {
    let r = rig(&[&commit(files)], &answer("pr_opened", ""));
    let mut p = piece();
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let (req, mut s) = request(&r, RunKind::Build, p);
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened, "{:?}", end.result);
    end
}

fn paths(end: &RunEnd) -> Vec<String> {
    (end.result["diff"]["files"].as_array().unwrap().iter())
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_approved_run_sends_the_real_diffs_facts_and_never_a_verdict_of_its_own() {
    let end = build("echo 'fn a() {}' > a.rs", json!({})).await;
    assert!(end.result["approved"].is_string());
    assert!(end.result.get("held").is_none(), "the store decides: {:?}", end.result);
    assert_eq!(paths(&end), ["a.rs"]);
    assert_eq!(end.result["diff"]["files"][0]["added"], 1);
    assert_eq!(end.result["diff"]["newDependencies"], false);
    assert!(end.result["diff"].get("size").is_none() && end.result["diff"]["files"][0].get("addedText").is_none());
}

#[tokio::test]
async fn a_migrations_added_text_goes_with_its_file_for_the_store_to_read() {
    let end = build(MIGRATION, json!({})).await;
    assert_eq!(paths(&end), ["db/migrations/1.sql"]);
    assert_eq!(end.result["diff"]["files"][0]["addedText"], "+DROP TABLE t;");
    assert!(end.result.get("held").is_none());
}

#[tokio::test]
async fn a_new_dependency_is_a_fact() {
    let end = build(
        // An empty node_modules (git keeps no empty folder) so the lane needs no npm install: CI's image has no npm.
        "mkdir -p node_modules; echo '{\"dependencies\":{\"left-pad\":\"1\"}}' > package.json",
        json!({}),
    )
    .await;
    assert_eq!(end.result["diff"]["newDependencies"], true);
}

#[tokio::test]
async fn every_changed_file_is_listed_and_the_pieces_size_goes_with_them() {
    let end = build(
        "mkdir -p src/auth; echo x > src/auth/login.rs; seq 1 200 > big.txt",
        json!({ "size": "S" }),
    )
    .await;
    let mut got = paths(&end);
    got.sort();
    assert_eq!(got, ["big.txt", "src/auth/login.rs"]);
    assert_eq!(end.result["diff"]["size"], "S");
    let end = build("echo x > a.rs", json!({ "size": "huge" })).await;
    assert!(end.result["diff"].get("size").is_none(), "an unknown size is not sent");
}

#[tokio::test]
async fn a_review_that_does_not_approve_sends_no_diff() {
    let r = rig(&[&commit(MIGRATION)], &answer("pr_opened", ""));
    script_review(&r, 1, "needs_human");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(end.result.get("diff").is_none() && end.result.get("approved").is_none());
}
