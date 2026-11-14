//! The model a piece run picks (docs/specs/agent/07.1-model-tiers.md "First tries are written on Sonnet").
use super::*;

fn build_piece(extra: Value) -> RunPiece {
    let mut p = json!({ "title": "t" });
    p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(p).unwrap()
}

fn model_of(piece: &RunPiece) -> &'static str {
    model_for(None, crate::tiers::OPUS, &piece_item(piece), is_retry(piece))
}

#[test]
fn a_retried_or_big_piece_keeps_opus() {
    assert_eq!(model_of(&build_piece(json!({}))), "sonnet");
    let failed = json!({ "attempts": [{ "kind": "build", "outcome": "failed" }] });
    assert_eq!(model_of(&build_piece(failed)), "opus");
    let limited = json!({ "attempts": [{ "kind": "build", "outcome": "limited" }] });
    assert_eq!(model_of(&build_piece(limited)), "sonnet");
    assert_eq!(model_of(&build_piece(json!({ "size": "XL" }))), "opus");
    let tidy: Chore = chore_of(&build_piece(json!({ "chore": { "kind": "tidy" } }))).unwrap();
    let item = piece_item(&build_piece(json!({})));
    assert_eq!(
        model_for(Some(&tidy), crate::tiers::OPUS, &item, true),
        crate::tiers::chore_tier()
    );
}
// A resumed session keeps the backend it started on.
use crate::local_model::LocalModel;

#[derive(Clone, Copy)]
enum Fix {
    Gate,
    Review,
}

/// What a run's two implementer sessions ran on: each session's `--model … BASE=<local url or empty>` line, and the
/// model the second one was listed with while it ran.
struct Seen {
    lines: Vec<String>,
    second_model: String,
}

/// Runs a piece on the `local` engine (`wants_local`) with the local model `up` for the first session; `down_after`
/// takes it down while that session runs, `up_after` brings it up. The first session is followed by a `fix` round.
async fn run_two(fix: Fix, wants_local: bool, up: bool, flip: Option<bool>) -> Seen {
    let local = if up { LocalModel::up() } else { LocalModel::off() };
    if !up {
        local.mark_down_for_test();
    }
    let sleepy = format!("sleep 2; {FIXED}");
    let sleepy_b = format!("sleep 2; {COMMIT_B}");
    let mut r = match fix {
        Fix::Gate => {
            let c = check("needs-fix", BROKEN_BY_BAD, true);
            rig_checked(&[&format!("sleep 2; {BAD}"), &sleepy], vec![c], 3)
        }
        Fix::Review => {
            let r = rig(&[&format!("sleep 2; {COMMIT}"), &sleepy_b], &answer("pr_opened", ""));
            script_review(&r, 1, "request_changes");
            r
        }
    };
    Arc::get_mut(&mut r.starter).unwrap().local = local.clone();
    // Record the local base URL each implementer session was given next to its arguments.
    let bin = r.root.join(fake_name());
    let script = std::fs::read_to_string(&bin).unwrap().replace(
        "echo \"$@\" >> \"$R/args_all\"",
        "echo \"$@ BASE=$ANTHROPIC_BASE_URL\" >> \"$R/args_all\"",
    );
    crate::test_support::write_executable(&bin, &script);
    let mut p = piece();
    if wants_local {
        p["engine"] = json!("local");
    }
    let (req, mut s) = request(&r, RunKind::Build, p);
    r.starter.start(req).unwrap();
    let args = r.root.join("args_all");
    let lines = |n: usize| std::fs::read_to_string(&args).is_ok_and(|t| t.lines().count() >= n);
    while !lines(1) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    match flip {
        Some(true) => local.mark_up_for_test(),
        Some(false) => local.mark_down_for_test(),
        None => {}
    }
    while !lines(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let rows = r.starter.sessions.snapshot();
    let second_model = rows.first().map(|r| r.info.model.clone()).unwrap_or_default();
    end_of(&mut s.ends).await;
    let lines = std::fs::read_to_string(&args)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    Seen { lines, second_model }
}

const LOCAL_URL: &str = "BASE=http://127.0.0.1:11436";
#[cfg(unix)]
#[tokio::test]
async fn a_local_first_session_resumes_its_gate_fix_and_review_changes_round_on_local() {
    for fix in [Fix::Gate, Fix::Review] {
        let seen = run_two(fix, true, true, None).await;
        assert!(seen.lines[0].contains(LOCAL_URL), "{:?}", seen.lines);
        assert!(
            seen.lines[1].contains("--resume") && seen.lines[1].contains("--model sonnet"),
            "{:?}",
            seen.lines
        );
        assert!(seen.lines[1].contains(LOCAL_URL), "{:?}", seen.lines);
        assert_eq!(seen.second_model, "local");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn with_the_model_down_before_the_fix_round_it_resumes_on_the_anthropic_tier() {
    for fix in [Fix::Gate, Fix::Review] {
        let seen = run_two(fix, true, true, Some(false)).await;
        assert!(seen.lines[0].contains(LOCAL_URL), "{:?}", seen.lines);
        assert!(
            seen.lines[1].contains("--model sonnet") && !seen.lines[1].contains(LOCAL_URL),
            "{:?}",
            seen.lines
        );
        assert_eq!(seen.second_model, "sonnet");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn an_anthropic_first_session_never_resumes_local() {
    for fix in [Fix::Gate, Fix::Review] {
        // The model was down when it started and is up for the fix round; `wants_local` is set.
        let seen = run_two(fix, true, false, Some(true)).await;
        assert!(!seen.lines[0].contains(LOCAL_URL), "{:?}", seen.lines);
        assert!(!seen.lines[1].contains(LOCAL_URL), "{:?}", seen.lines);
        assert_eq!(seen.second_model, "sonnet");
    }
}
