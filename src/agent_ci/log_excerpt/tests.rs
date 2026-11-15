use super::*;

fn long_log() -> String {
    let mut l: Vec<String> = [
        "# Subtest: breaks",
        "not ok 3 - breaks",
        "  ---",
        "  error: 'expected 1'",
        "  ...",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    l.extend((0..50000).map(|i| format!("ok {i} - passing")));
    l.push("# fail 1".into());
    l.join("\n")
}

#[test]
fn a_short_log_is_posted_whole() {
    assert_eq!(excerpt("a\nb\nnot ok 1 - x\n", MAX_LINES), "a\nb\nnot ok 1 - x");
}

#[test]
fn a_long_log_keeps_an_early_not_ok_with_its_diagnostics_and_the_end() {
    let out = excerpt(&long_log(), MAX_LINES);
    assert!(
        out.contains("# Subtest: breaks\nnot ok 3 - breaks\n  ---\n  error: 'expected 1'\n  ..."),
        "{}",
        &out[..200]
    );
    assert!(Regex::new(r"\.\.\. \(\d+ lines left out\)").unwrap().is_match(&out));
    assert!(out.ends_with("# fail 1"));
    assert!(out.split('\n').count() <= 205);
    assert!(out.len() < 60000);
}

#[test]
fn many_failures_and_very_long_lines_stay_under_the_comment_limit() {
    let wide = "x".repeat(2000);
    let log: Vec<String> = (0..5000).map(|i| format!("not ok {i} {wide}")).collect();
    let out = excerpt(&log.join("\n"), MAX_LINES);
    assert!(out.chars().count() < 61000);
    assert!(out.starts_with("... (earlier lines left out)\n"));
}

#[test]
fn many_scattered_failures_keep_the_excerpt_within_max_lines_markers_counted() {
    let log: Vec<String> = (0..15000)
        .map(|i| {
            if i % 100 == 0 {
                format!("Error: boom {i}")
            } else {
                format!("ok {i}")
            }
        })
        .collect();
    let out = excerpt(&log.join("\n"), MAX_LINES);
    assert!(out.split('\n').count() <= MAX_LINES);
    assert!(out.ends_with("ok 14999"));
}

#[test]
fn the_comment_is_the_excerpt() {
    let (lead, log) = comment_log_text(&long_log());
    assert_eq!(lead, "The failing lines and last lines of its log:");
    assert!(log.contains("not ok 3 - breaks"));
}

#[test]
fn carriage_returns_go_and_each_failure_kind_matches() {
    for l in [
        "  not ok 1",
        "x ✖ y",
        "error TS2304: x",
        "Error: x",
        "npm ERR! x",
        "npm error x",
        "##[error]x",
    ] {
        assert!(fail(l), "{l}");
    }
    assert!(!fail("ok 1 - fine") && !fail("an error occurred"));
    assert_eq!(excerpt("a\r\nb\r\n", MAX_LINES), "a\nb");
}

#[test]
fn a_failed_test_line_survives_a_log_full_of_printed_errors() {
    let mut l: Vec<String> = (0..400).map(|i| format!("Error: expected noise {i}")).collect();
    l.push("✖ deploy-watch lists every file the agent ships (2ms)".into());
    l.push("  AssertionError: shared/ciAgent.ts is missing".into());
    l.extend((0..1000).map(|i| format!("✔ passing {i}")));
    let out = excerpt(&l.join("\n"), MAX_LINES);
    assert!(
        out.contains(
            "✖ deploy-watch lists every file the agent ships (2ms)\n  AssertionError: shared/ciAgent.ts is missing"
        ),
        "{out}"
    );
    assert!(out.contains("Error: expected noise 0"));
    assert!(out.split('\n').count() <= MAX_LINES);
}
