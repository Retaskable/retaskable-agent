//! A review that hits its engine's usage limit (docs/specs/engines/03-choosing-the-engine.md section 3.5).
use super::*;
use crate::text::is_blank;

/// A build whose review roles are on Cursor, whose stub ends every session at its usage limit.
fn cursor_review_at_its_limit(
    used: Vec<crate::engines::EngineId>,
    out: &[crate::engines::EngineId],
    fall: bool,
) -> Rig {
    let mut r = rig(&[COMMIT], &answer("pr_opened", ""));
    placing(&mut r);
    stub(&mut r, "RETASKABLE_CURSOR", "cursor");
    let path = r.root.join("stub-cursor");
    crate::test_support::write_executable(
        &path,
        &format!(
            "#!/bin/sh\ncat > /dev/null\necho \"$@\" >> '{}'\necho '{}'\n",
            r.root.join("cursor.log").display(),
            r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"limit","session_id":"c"}"#
        ),
    );
    let starter = Arc::get_mut(&mut r.starter).unwrap();
    starter.flags = Arc::new(crate::flags::Flags::fixed(&[
        "engine-placement",
        "run-limits-and-causes",
    ]));
    starter.choice = serde_json::from_value(json!({ "roles": { "review": "cursor" } })).unwrap();
    starter.projects[0].review.min_scout_diff_lines = 0;
    r.fall = fall.then(|| crate::orchestrator::driver::FallOver {
        out: out.iter().copied().collect(),
        used,
    });
    r
}

async fn end_with_fall(r: &Rig) -> RunEnd {
    let mut p = piece();
    p["engine"] = json!("claude");
    let (mut req, mut s) = request(r, RunKind::Build, p);
    req.context.fall = r.fall.clone();
    r.starter.start(req).unwrap();
    end_of(&mut s.ends).await
}

#[tokio::test]
async fn a_review_that_hits_cursors_limit_re_runs_on_claude_and_the_run_ends_pr_opened() {
    use crate::engines::EngineId::{Claude, Cursor};
    let r = cursor_review_at_its_limit(vec![Cursor, Claude], &[], true);
    assert_eq!(end_with_fall(&r).await.outcome, RunOutcome::PrOpened);
    assert!(r.root.join("cursor.log").exists(), "the review began on Cursor");
    assert!(
        r.root.join("reviewer1.txt").exists(),
        "the final reviewer ran on Claude"
    );
}

#[tokio::test]
async fn a_review_that_hits_a_limit_with_no_other_engine_ends_limited_with_its_engine() {
    use crate::engines::EngineId::Cursor;
    let r = cursor_review_at_its_limit(vec![Cursor], &[], true);
    let end = end_with_fall(&r).await;
    assert!(
        matches!(end.outcome, RunOutcome::Limited { ref engine, .. } if engine == "cursor"),
        "{:?}",
        end.outcome
    );
    assert!(!r.root.join("reviewer1.txt").exists());
}

#[tokio::test]
async fn a_review_starts_on_claude_when_cursor_was_already_out() {
    use crate::engines::EngineId::{Claude, Cursor};
    let r = cursor_review_at_its_limit(vec![Cursor, Claude], &[Cursor], true);
    assert_eq!(end_with_fall(&r).await.outcome, RunOutcome::PrOpened);
    let log = std::fs::read_to_string(r.root.join("cursor.log")).unwrap_or_default();
    assert!(is_blank(&log), "nothing started on Cursor: {log}");
    assert!(r.root.join("reviewer1.txt").exists());
}

#[tokio::test]
async fn with_the_fall_over_flag_off_a_review_at_a_limit_fails_as_before() {
    let r = cursor_review_at_its_limit(vec![], &[], false);
    let end = end_with_fall(&r).await;
    assert!(matches!(end.outcome, RunOutcome::Failed(_)), "{:?}", end.outcome);
    assert!(!r.root.join("reviewer1.txt").exists());
}
