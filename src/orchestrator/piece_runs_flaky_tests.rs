//! A `fix` run whose session found the failure is not the change's (`notTheChange`): it commits nothing and ends `failed`
//! with the failing names, so the store can tell a flaky test from a broken change.
use super::*;

/// A piece with a pushed branch, as a fix run gets one.
fn pushed_piece(r: &Rig) -> Value {
    let repo = r.root.join("repo");
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["checkout", "-q", "-b", "pushed-branch"]);
    std::fs::write(repo.join("pushed.rs"), "fn p() {}").unwrap();
    git(&["add", "-A"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "pushed"]);
    git(&["push", "-q", "origin", "pushed-branch"]);
    git(&["checkout", "-q", "main"]);
    let mut p = piece();
    p["branch"] = json!("pushed-branch");
    p["next"] = json!({ "what": "Fix the red check." });
    p
}

async fn run(kind: RunKind, steps: &[&str], extra: &str, status: &str) -> RunEnd {
    let r = rig(steps, &answer(status, extra));
    let p = if kind == RunKind::Fix {
        pushed_piece(&r)
    } else {
        piece()
    };
    let (req, mut s) = request(&r, kind, p);
    r.starter.start(req).unwrap();
    end_of(&mut s.ends).await
}

const FLAGGED: &str = r#","notTheChange":true,"failing":["timing_test","  other  ",""]"#;

#[tokio::test]
async fn a_flagged_fix_with_no_commit_ends_failed_with_the_failing_names() {
    let end = run(RunKind::Fix, &["true"], FLAGGED, "failed").await;
    assert_eq!(end.outcome, RunOutcome::Failed("s".into()), "{:?}", end.result);
    assert_eq!(
        end.result,
        json!({ "notTheChange": true, "failing": ["timing_test", "other"] })
    );
}

#[tokio::test]
async fn a_flagged_fix_that_committed_goes_the_ordinary_way() {
    let end = run(RunKind::Fix, &[COMMIT], FLAGGED, "pr_opened").await;
    assert_eq!(end.outcome, RunOutcome::PrOpened, "{:?}", end.result);
    assert!(end.result.get("notTheChange").is_none());
    let end = run(RunKind::Fix, &[COMMIT], FLAGGED, "failed").await;
    assert_eq!(end.outcome, RunOutcome::Failed("s".into()));
    assert!(end.result.get("notTheChange").is_none(), "{:?}", end.result);
}

#[tokio::test]
async fn a_flagged_fix_with_uncommitted_work_goes_the_ordinary_way() {
    let end = run(RunKind::Fix, &["echo x > loose.rs"], FLAGGED, "failed").await;
    assert!(end.result.get("notTheChange").is_none(), "{:?}", end.result);
}

#[tokio::test]
async fn a_flag_on_a_build_run_is_ignored() {
    let end = run(RunKind::Build, &["true"], FLAGGED, "failed").await;
    assert_eq!(end.outcome, RunOutcome::Failed("s".into()));
    assert!(end.result.get("notTheChange").is_none(), "{:?}", end.result);
}

#[test]
fn the_failing_list_is_cut_to_ten_entries_of_300_characters() {
    let r: crate::implementer::ImplementerResult = serde_json::from_value(json!({
        "status": "failed", "summary": "  ", "notTheChange": true,
        "failing": (0..15).map(|i| if i == 0 { "x".repeat(500) } else { format!("t{i}") }).collect::<Vec<_>>(),
    }))
    .unwrap();
    let (outcome, result) = not_the_change_end(&r);
    assert_eq!(
        outcome,
        RunOutcome::Failed("The failing check is not caused by this change".into())
    );
    let failing = result["failing"].as_array().unwrap();
    assert_eq!(failing.len(), 10);
    assert_eq!(failing[0].as_str().unwrap().len(), 300);
}

#[test]
fn the_wire_shape_is_why_not_the_change_and_failing_under_result() {
    let (outcome, result) = not_the_change_end(
        &serde_json::from_value(
            json!({ "status": "failed", "summary": "Flaky", "notTheChange": true, "failing": ["a"] }),
        )
        .unwrap(),
    );
    let args = crate::backlog::runs::run_done_args("tok", &outcome, result);
    assert_eq!(
        args,
        json!({ "token": "tok", "outcome": "failed",
                "result": { "why": "Flaky", "notTheChange": true, "failing": ["a"] } })
    );
}
