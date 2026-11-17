//! Tier escalation in a `build` run (docs/specs/pieces/03-behavior.md "Approval reuse and tier escalation"): what
//! the approved diff holds the piece for, sent as `result.held`.
use super::*;

fn commit(files: &str) -> String {
    format!("{files}; git add -A; git -c user.name=t -c user.email=t@t commit -qm change")
}

const MIGRATION: &str = "mkdir -p db/migrations; echo 'DROP TABLE t;' > db/migrations/1.sql";
const ADD_MIGRATION: &str = "mkdir -p db/migrations; echo 'CREATE TABLE t(a int);' > db/migrations/1.sql";

async fn build(files: &str, extra: Value, profile: Option<Value>) -> RunEnd {
    let r = rig(&[&commit(files)], &answer("pr_opened", ""));
    let mut p = piece();
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    let (mut req, mut s) = request(&r, RunKind::Build, p);
    req.context.risk_profile =
        profile.and_then(|v| crate::change_tier::RiskProfile::from_values(&json!({ "riskProfile": v })));
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert_eq!(end.outcome, RunOutcome::PrOpened, "{:?}", end.result);
    end
}

#[tokio::test]
async fn a_small_plain_change_sends_no_held_and_an_approved_one() {
    let end = build("echo 'fn a() {}' > a.rs", json!({}), None).await;
    assert!(end.result.get("held").is_none(), "{:?}", end.result);
    assert!(end.result["approved"].is_string());
}

#[tokio::test]
async fn a_migration_that_may_drop_data_holds_the_piece_with_its_reason() {
    let end = build(MIGRATION, json!({}), None).await;
    assert_eq!(
        end.result["held"],
        "a migration that may drop or alter data (db/migrations/1.sql)"
    );
}

#[tokio::test]
async fn a_new_dependency_holds_the_piece() {
    let end = build(
        // An empty node_modules (git keeps no empty folder) so the lane needs no npm install: CI's image has no npm.
        "mkdir -p node_modules; echo '{\"dependencies\":{\"left-pad\":\"1\"}}' > package.json",
        json!({}),
        None,
    )
    .await;
    assert_eq!(end.result["held"], "it adds a dependency");
}

#[tokio::test]
async fn auth_paths_hold_the_piece_and_several_reasons_are_joined() {
    let end = build(
        &format!("mkdir -p src/auth; echo x > src/auth/login.rs; {MIGRATION}"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        end.result["held"],
        "a migration that may drop or alter data (db/migrations/1.sql); the change touches sign-in code (src/auth/login.rs)"
    );
}

#[tokio::test]
async fn held_is_cut_to_500_characters() {
    let names: String = (0..40)
        .map(|n| format!("echo x > src/auth/{n:02}_{}.rs; ", "a".repeat(30)))
        .collect();
    let end = build(&format!("mkdir -p src/auth; {names}true"), json!({}), None).await;
    let held = end.result["held"].as_str().unwrap();
    assert_eq!(held.chars().count(), 500);
    assert!(
        held.starts_with("the change touches sign-in code (src/auth/00_"),
        "{held}"
    );
}

#[tokio::test]
async fn the_pieces_size_sets_how_big_is_too_big() {
    let lines = "seq 1 200 > big.txt";
    let end = build(lines, json!({ "size": "S" }), None).await;
    assert_eq!(end.result["held"], "200 lines changed, more than a S piece");
    let end = build(lines, json!({ "size": "M" }), None).await;
    assert!(end.result.get("held").is_none(), "{:?}", end.result);
}

#[tokio::test]
async fn the_risk_profile_the_run_was_handed_decides_which_kinds_hold() {
    // Early (no profile): a database change goes live first. A Prototype project lets it ship.
    let end = build(ADD_MIGRATION, json!({}), None).await;
    assert_eq!(end.result["held"], "a migration (db/migrations/1.sql)");
    let end = build(ADD_MIGRATION, json!({}), Some(json!({ "stage": "prototype" }))).await;
    assert!(end.result.get("held").is_none(), "{:?}", end.result);
    let live = json!({ "stage": "live", "choices": { "live": { "db": "ship" } } });
    let end = build(ADD_MIGRATION, json!({}), Some(live)).await;
    assert!(end.result.get("held").is_none(), "{:?}", end.result);
}

#[tokio::test]
async fn a_review_that_does_not_approve_sends_no_held() {
    let r = rig(&[&commit(MIGRATION)], &answer("pr_opened", ""));
    script_review(&r, 1, "needs_human");
    let (req, mut s) = request(&r, RunKind::Build, piece());
    r.starter.start(req).unwrap();
    let end = end_of(&mut s.ends).await;
    assert!(end.result.get("held").is_none() && end.result.get("approved").is_none());
}
