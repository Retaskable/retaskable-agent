use super::*;
use serde_json::json;

fn status(context: &str, state: &str, at: Option<&str>) -> Status {
    serde_json::from_value(
        json!({ "context": context, "state": state, "target_url": format!("https://ci/{context}"), "updated_at": at }),
    )
    .unwrap()
}

fn run(name: &str, status: &str, conclusion: Option<&str>, at: Option<&str>) -> Run {
    serde_json::from_value(json!({ "name": name, "status": status, "conclusion": conclusion,
        "html_url": format!("https://github.com/o/r/actions/runs/42/job/{name}"), "completed_at": at }))
    .unwrap()
}

#[test]
fn no_status_and_no_run_is_none_and_skipped_runs_are_left_out() {
    assert_eq!(commit_checks_of(&[], &[]).state, CommitState::None);
    let runs = [
        run("a", "completed", Some("neutral"), None),
        run("b", "completed", Some("skipped"), None),
    ];
    // Checks that all skipped reported, so there is nothing to wait for.
    assert_eq!(commit_checks_of(&[], &runs).state, CommitState::Pass);
    // A skipped run beside a passing one changes nothing.
    let runs = [
        run("a", "completed", Some("skipped"), None),
        run("c", "completed", Some("success"), None),
    ];
    assert_eq!(commit_checks_of(&[], &runs).state, CommitState::Pass);
}

#[test]
fn a_run_not_completed_or_a_pending_status_is_pending_and_a_failure_outranks_it() {
    let pending = commit_checks_of(&[status("s", "success", None)], &[run("a", "in_progress", None, None)]);
    assert_eq!((pending.state, pending.failed.len()), (CommitState::Pending, 0));
    let pending = commit_checks_of(&[status("s", "pending", None)], &[]);
    assert_eq!(pending.state, CommitState::Pending);
    let both = commit_checks_of(
        &[status("s", "pending", None)],
        &[run("a", "completed", Some("failure"), None)],
    );
    assert_eq!(both.state, CommitState::Fail);
}

#[test]
fn a_failed_status_or_run_counts_and_a_cancelled_run_counts_as_failed_with_when_it_ended() {
    let st = commit_checks_of(
        &[
            status("lint", "failure", Some("2026-11-01T10:00:00Z")),
            status("ok", "success", None),
        ],
        &[
            run("test", "completed", Some("timed_out"), Some("2026-11-01T10:05:00Z")),
            run("e2e", "completed", Some("cancelled"), Some("2026-11-01T10:06:00Z")),
            run("build", "completed", Some("success"), Some("2026-11-01T09:00:00Z")),
        ],
    );
    assert_eq!(st.state, CommitState::Fail);
    let got: Vec<_> = st
        .failed
        .iter()
        .map(|f| (f.name.as_str(), f.cancelled, f.completed_at.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("lint", false, Some("2026-11-01T10:00:00Z")),
            ("test", false, Some("2026-11-01T10:05:00Z")),
            ("e2e", true, Some("2026-11-01T10:06:00Z")),
        ]
    );
    assert_eq!(st.failed[0].link, "https://ci/lint");
    assert_eq!(ended_at(&st.failed), Some(1_793_527_560_000));
    // A pending status has no end; neither is the time of a check that never says.
    let p = commit_checks_of(&[status("s", "pending", Some("2026-11-01T10:00:00Z"))], &[]);
    assert!(p.failed.is_empty());
    assert_eq!(
        ended_at(&[FailedCheck {
            name: "x".into(),
            link: String::new(),
            cancelled: false,
            completed_at: None
        }]),
        None
    );
}

#[test]
fn everything_green_is_a_pass() {
    let st = commit_checks_of(
        &[status("s", "success", None)],
        &[run("a", "completed", Some("success"), None)],
    );
    assert_eq!(
        st,
        CommitChecks {
            state: CommitState::Pass,
            failed: vec![]
        }
    );
}

fn failed(name: &str, link: &str) -> FailedCheck {
    FailedCheck {
        name: name.into(),
        link: link.into(),
        cancelled: false,
        completed_at: None,
    }
}

#[test]
fn the_note_names_each_check_and_quotes_the_logs_that_were_read() {
    let f = [failed("test", "https://x/1"), failed("lint", "https://x/2")];
    let note = failed_note(&f, &[Some("boom".into()), None], "pull request", None);
    assert_eq!(
        note,
        "CI failed on your pull request:\n\n- test: https://x/1\n- lint: https://x/2\n\nThe failing lines and end of test's log:\n\n```\nboom\n```\n\nFix what failed, commit, and return the result again."
    );
    let none = failed_note(&f[..1], &[None], "pull request", None);
    assert!(
        none.contains("No log was readable on this computer, so reproduce the failure first"),
        "{none}"
    );
    assert!(none.ends_with("return the result again."), "{none}");
    assert_eq!(
        failed_note(&f[..1], &[Some(String::new())], "PR", None),
        failed_note(&f[..1], &[], "PR", None)
    );
}

#[test]
fn a_note_over_its_budget_keeps_the_failure_lines_and_the_end_and_the_closing_line() {
    let mut log: Vec<String> = (1..=400).map(|i| format!("passing line {i}")).collect();
    log[30] = "AssertionError is not a failure line but Error: the real failure".into();
    let log = log.join("\n");
    let f = [failed("test", "https://x/1")];
    let note = failed_note(&f, &[Some(log.clone())], "pull request", Some(WHAT_MAX));
    assert!(note.chars().count() <= WHAT_MAX, "{}", note.chars().count());
    assert!(note.contains("Error: the real failure"), "{note}");
    assert!(note.contains("passing line 400"), "{note}");
    assert!(!note.contains("passing line 100\n"), "{note}");
    assert!(note.ends_with("Fix what failed, commit, and return the result again."));
    // Within the budget it is left as it is.
    assert_eq!(
        failed_note(&f, &[Some("x".into())], "PR", Some(WHAT_MAX)),
        failed_note(&f, &[Some("x".into())], "PR", None)
    );
    // A log of passing lines only falls back to its end.
    let plain: Vec<String> = (1..=400).map(|i| format!("line {i}")).collect();
    let note = failed_note(&f, &[Some(plain.join("\n"))], "PR", Some(1000));
    assert!(
        note.chars().count() <= 1000 && note.contains("line 400") && !note.contains("line 1\n"),
        "{note}"
    );
}

#[test]
fn keep_end_marks_what_it_leaves_out() {
    assert_eq!(keep_end("a\nb", 10), "a\nb");
    let a = "a".repeat(20);
    let out = keep_end(&format!("{a}\n{a}\nthree\nfour"), 44);
    assert_eq!(out, "... (2 earlier lines left out)\nthree\nfour");
    assert!(keep_end(&format!("{a}{a}\nthree"), 37).starts_with("... (1 earlier line left out)\n"));
    assert_eq!(keep_end(&"z".repeat(100), 40).chars().count(), 40);
    assert_eq!(keep_end("abc", 0), "");
}

// What `gh api --paginate --jq .statuses[]` prints: one object per line.
const STATUS_JSON: &str =
    r#"{"context":"deploy","state":"failure","target_url":"https://ci/deploy","updated_at":"2026-11-01T10:00:00Z"}"#;
const RUNS_JSON: &str = r#"{"name":"test","status":"completed","conclusion":"failure","html_url":"https://github.com/o/r/actions/runs/42/job/9","completed_at":"2026-11-01T10:05:00Z"}
{"name":"docs","status":"completed","conclusion":"skipped","html_url":"https://github.com/o/r/actions/runs/42/job/10","completed_at":"2026-11-01T10:05:00Z"}"#;

/// A fake `gh` for commit `abc1234` of `o/r`: the two reads, a run's failed log and a CI comment. A read is its
/// first page's lines; later pages are printed only when `--paginate` was asked, as `gh` does.
fn fake_gh_pages(dir: &Path, statuses: &[&str], runs: &[&str]) -> std::path::PathBuf {
    let gh = dir.join("fake_gh.sh");
    let pages = |name: &str, pages: &[&str]| {
        let first = format!("cat <<'EOF'\n{}\nEOF\n", pages[0]);
        let more: String = pages[1..]
            .iter()
            .map(|p| format!("case \"$*\" in *--paginate*) cat <<'EOF'\n{p}\nEOF\n;; esac\n"))
            .collect();
        format!("\"api repos/o/r/commits/abc1234/{name}?per_page=100\"*) {first}{more};;\n")
    };
    let script = format!(
        "#!/bin/sh\n[ -n \"$1\" ] || exit 0\ncase \"$*\" in\n{}{}\
         \"run view 42 --repo o/r --log-failed\") i=1; while [ $i -le 300 ]; do echo \"line $i\"; i=$((i+1)); done; echo 'npm ERR! it broke';;\n\
         \"api repos/o/r/issues/comments/9 --jq .body\") printf 'Log:\\r\\n```\\r\\nthe comment log\\r\\n```\\r\\nbye\\r\\n';;\n\
         *) echo \"unexpected: $*\" >&2; exit 1;;\nesac\n",
        pages("status", statuses),
        pages("check-runs", runs),
    );
    crate::test_support::write_executable(&gh, &script);
    gh
}

fn fake_gh(dir: &Path, statuses: &str, runs: &str) -> std::path::PathBuf {
    fake_gh_pages(dir, &[statuses], &[runs])
}

#[cfg(unix)]
#[test]
fn commit_checks_reads_the_statuses_and_the_runs_through_gh_api() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake_gh(dir.path(), STATUS_JSON, RUNS_JSON);
    let st = commit_checks(&gh, dir.path(), "o/r", "abc1234", &|_| {}).unwrap();
    assert_eq!(st.state, CommitState::Fail);
    assert_eq!(
        st.failed.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["deploy", "test"]
    );
    // A commit with nothing reported at all reads as none.
    let gh = fake_gh(dir.path(), "", "");
    assert_eq!(
        commit_checks(&gh, dir.path(), "o/r", "abc1234", &|_| {}).unwrap().state,
        CommitState::None
    );
}

#[cfg(unix)]
#[test]
fn a_failed_read_is_an_error_naming_it_never_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.sh");
    crate::test_support::write_executable(
        &bad,
        "#!/bin/sh\n[ -n \"$1\" ] || exit 0\necho 'HTTP 403: Resource not accessible' >&2\nexit 1\n",
    );
    let e = commit_checks(&bad, dir.path(), "o/r", "abc1234def", &|_| {})
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("commits/abc1234/status failed") && e.contains("HTTP 403"),
        "{e}"
    );
    let junk = dir.path().join("junk.sh");
    crate::test_support::write_executable(&junk, "#!/bin/sh\n[ -n \"$1\" ] || exit 0\necho 'not json'\n");
    assert!(commit_checks(&junk, dir.path(), "o/r", "abc1234", &|_| {}).is_err());
}

#[cfg(unix)]
#[test]
fn a_log_comes_from_the_ci_comment_or_the_actions_run_and_is_none_for_any_other_link() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake_gh(dir.path(), "[]", "[]");
    let log = |link: &str| ci_log(&gh, dir.path(), "o/r", &failed("t", link), &|_| {});
    let run_log = log("https://github.com/o/r/actions/runs/42/job/9").unwrap();
    assert!(
        run_log.contains("npm ERR! it broke") && run_log.contains("line 300"),
        "{run_log}"
    );
    assert!(run_log.lines().count() <= LOG_TAIL_LINES, "{}", run_log.lines().count());
    assert_eq!(
        log("https://github.com/o/r/pull/3#issuecomment-9").as_deref(),
        Some("the comment log")
    );
    assert_eq!(log("https://workers.example/build/1"), None);
    // A comment gh can't read, and a run it can't read, are no log.
    assert_eq!(log("https://github.com/o/r/pull/3#issuecomment-10"), None);
    assert_eq!(log("https://github.com/o/r/actions/runs/43/job/1"), None);
}

#[cfg(unix)]
#[test]
fn read_ci_checks_gives_the_facts_of_each_state() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake_gh(dir.path(), STATUS_JSON, RUNS_JSON);
    let c = read_ci_checks(&gh, dir.path(), "o/r", "abc1234", &|_| {}).unwrap();
    assert_eq!(c.state, CiState::Fail);
    assert_eq!(c.ended_at, Some(1_793_527_500_000));
    assert_eq!(c.logs.len(), 2);
    // The status has a CI link gh knows no log for; the run's log was read.
    assert!(c.logs[0].is_none() && c.logs[1].as_deref().unwrap().contains("npm ERR!"));
    assert!(
        c.what
            .starts_with("CI failed on your pull request:\n\n- deploy: https://ci/deploy\n- test: "),
        "{}",
        c.what
    );
    assert!(c.what.chars().count() <= WHAT_MAX);

    let pending = r#"{"name":"a","status":"queued","conclusion":null,"html_url":"","completed_at":null}"#;
    let gh = fake_gh(dir.path(), "", pending);
    let c = read_ci_checks(&gh, dir.path(), "o/r", "abc1234", &|_| {}).unwrap();
    assert_eq!(
        (c.state, c.what.as_str(), c.logs.len(), c.ended_at),
        (CiState::Pending, "", 0, None)
    );

    // Nothing reported yet is pending, not a pass.
    let gh = fake_gh(dir.path(), "", "");
    let c = read_ci_checks(&gh, dir.path(), "o/r", "abc1234", &|_| {}).unwrap();
    assert_eq!(c.state, CiState::Pending);
}

#[cfg(unix)]
#[test]
fn a_failure_on_the_second_page_is_read_not_taken_for_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let ok = r#"{"name":"a","status":"completed","conclusion":"success","html_url":"","completed_at":null}"#;
    let bad = r#"{"name":"z","status":"completed","conclusion":"failure","html_url":"","completed_at":null}"#;
    let gh = fake_gh_pages(dir.path(), &[""], &[ok, bad]);
    let st = commit_checks(&gh, dir.path(), "o/r", "abc1234", &|_| {}).unwrap();
    assert_eq!(st.state, CommitState::Fail);
    assert_eq!(st.failed[0].name, "z");
}

#[test]
fn keep_end_ignores_a_trailing_newline_when_the_last_line_alone_is_too_long() {
    // A log ends with a newline: the empty piece after it must not count as the last line, or nothing of the
    // real last line is shown.
    let out = keep_end(&format!("first\n{}\n", "z".repeat(100)), 40);
    assert_eq!(out.chars().count(), 40);
    assert!(out.ends_with("zzzz"), "{out}");
}

/// A fake `gh` that serves the two reads as conditional GETs: a 200 with an ETag, then a 304 for the same ETag, logging each call.
#[cfg(unix)]
fn fake_conditional_gh(dir: &Path, total: u32) -> std::path::PathBuf {
    let gh = dir.join("conditional_gh.sh");
    let script = format!(
        "#!/bin/sh\n[ -n \"$1\" ] || exit 0\necho \"$*\" >> \"{log}\"\n\
         case \"$*\" in\n*--paginate*) echo '{{\"context\":\"deploy\",\"state\":\"success\"}}';;\n\
         *if-none-match*) printf 'HTTP/2.0 304 Not Modified\\r\\nX-Ratelimit-Resource: core\\r\\n\\r\\n'; exit 1;;\n\
         *check-runs*) printf 'HTTP/2.0 200 OK\\r\\nEtag: \"r1\"\\r\\n\\r\\n{{\"total_count\":0,\"check_runs\":[]}}';;\n\
         *) printf 'HTTP/2.0 200 OK\\r\\nEtag: \"s1\"\\r\\n\\r\\n{{\"total_count\":{total},\"statuses\":[{{\"context\":\"deploy\",\"state\":\"success\"}}]}}';;\nesac\n",
        log = dir.join("calls.log").display()
    );
    crate::test_support::write_executable(&gh, &script);
    gh
}

#[cfg(unix)]
#[test]
fn a_commit_read_again_is_a_304_and_the_same_answer() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake_conditional_gh(dir.path(), 1);
    let read = || commit_checks(&gh, dir.path(), "cond/short", "abc1234", &|_| {}).unwrap();
    assert_eq!(read().state, CommitState::Pass);
    assert_eq!(read().state, CommitState::Pass);
    let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
    assert_eq!(calls.lines().count(), 4, "two reads, each asked twice: {calls}");
    assert_eq!(
        calls.lines().filter(|l| l.contains("if-none-match")).count(),
        2,
        "the second round sends its ETags: {calls}"
    );
    assert!(!calls.contains("--paginate"), "{calls}");
}

#[cfg(unix)]
#[test]
fn a_list_longer_than_the_page_is_read_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake_conditional_gh(dir.path(), 101);
    let st = commit_checks(&gh, dir.path(), "cond/long", "abc1234", &|_| {}).unwrap();
    assert_eq!(st.state, CommitState::Pass);
    let calls = std::fs::read_to_string(dir.path().join("calls.log")).unwrap();
    assert!(calls.lines().any(|l| l.contains("--paginate")), "{calls}");
}
