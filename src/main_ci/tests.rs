use super::*;

fn failed(name: &str, cancelled: bool) -> FailedCheck {
    FailedCheck {
        name: name.to_string(),
        link: format!("https://github.com/o/r/actions/runs/1/job/{name}"),
        cancelled,
        completed_at: Some("2026-11-01T10:05:00Z".to_string()),
    }
}

fn checks(state: CommitState, failed: Vec<FailedCheck>) -> std::result::Result<CommitChecks, String> {
    Ok(CommitChecks { state, failed })
}

#[test]
fn each_commit_state_maps_and_a_host_that_cannot_say_is_none() {
    let of = |c| MainCi::of("abc", c).state;
    assert_eq!(of(checks(CommitState::Pass, vec![])), MainCiState::Pass);
    assert_eq!(of(checks(CommitState::Pending, vec![])), MainCiState::Pending);
    assert_eq!(of(checks(CommitState::None, vec![])), MainCiState::None);
    assert_eq!(of(Err("no access".into())), MainCiState::None);
    assert_eq!(
        of(checks(CommitState::Fail, vec![failed("test", false)])),
        MainCiState::Fail
    );
}

#[test]
fn a_cancelled_run_is_no_failure_of_mains_and_only_cancelled_ones_are_none() {
    let some = MainCi::of(
        "abc",
        checks(CommitState::Fail, vec![failed("lint", true), failed("test", false)]),
    );
    assert_eq!(some.state, MainCiState::Fail);
    assert_eq!(some.failed, vec![failed("test", false)]);
    let only = MainCi::of("abc", checks(CommitState::Fail, vec![failed("lint", true)]));
    assert_eq!((only.state, only.failed.len()), (MainCiState::None, 0));
}

#[test]
fn it_is_settled_for_its_own_commit_unless_pending() {
    let pass = MainCi::of("abc", checks(CommitState::Pass, vec![]));
    assert!(pass.settled_for("abc"));
    assert!(!pass.settled_for("def"), "main moved");
    assert!(!MainCi::of("abc", checks(CommitState::Pending, vec![])).settled_for("abc"));
    assert!(MainCi::of("abc", Err("x".into())).settled_for("abc"));
}

#[test]
fn red_is_the_failures_only_for_the_commit_it_was_read_at() {
    let red = MainCi::of("abc", checks(CommitState::Fail, vec![failed("test", false)]));
    assert_eq!(red.red("abc").len(), 1);
    assert!(red.red("def").is_empty());
    assert!(
        MainCi::of("abc", checks(CommitState::Pass, vec![]))
            .red("abc")
            .is_empty()
    );
}

#[test]
fn the_fact_has_the_stores_shape() {
    let red = MainCi::of("abc", checks(CommitState::Fail, vec![failed("test", false)]));
    assert_eq!(
        red.to_fact(),
        json!({ "sha": "abc", "state": "fail", "failed": [
            { "name": "test", "link": "https://github.com/o/r/actions/runs/1/job/test", "completedAt": "2026-11-01T10:05:00Z" }
        ] })
    );
    assert_eq!(
        MainCi::of("abc", checks(CommitState::Pending, vec![])).to_fact(),
        json!({ "sha": "abc", "state": "pending" })
    );
}

#[test]
fn the_brief_names_each_check_with_its_link_and_log_or_says_it_could_not_be_read() {
    let failed = vec![failed("test", false), failed("lint", false)];
    let text = brief("0123456789abcdef", &failed, &[Some("error: boom".to_string()), None]);
    assert!(text.starts_with("CI on the code host failed on the base branch's latest commit (0123456), so every change after it starts from a red main:\n\n### test\n"));
    assert!(text.contains("job/test\n```\nerror: boom\n```"));
    assert!(text.contains(
        "### lint\nhttps://github.com/o/r/actions/runs/1/job/lint\n(Its log couldn't be read here: open the link.)"
    ));
    assert!(text.ends_with("say what you found in the pull request."));
    assert_eq!(brief("abc", &[], &[]), "");
}

#[test]
fn a_long_log_keeps_its_last_3000_characters() {
    let log = format!("{}{}", "a".repeat(5000), "END");
    let text = brief("abc", &[failed("t", false)], &[Some(log)]);
    assert!(text.contains("END\n```"));
    assert!(!text.contains(&"a".repeat(3000)), "cut to 3000 including the end");
}
