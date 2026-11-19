use super::*;

#[test]
fn a_conflict_is_named_plainly_with_the_stable_reason() {
    let log = "+ git merge\nCONFLICT (content): x\nerror: conflicts with main; merge main into the branch\n+ exit 1\n";
    let f = Failure::of(Step::Check, log);
    assert_eq!(f.summary, "conflicts with main");
    assert_eq!(f.reason, Some("conflict"));
    assert!(f.excerpt.contains("conflicts with main; merge main"));
}

#[test]
fn a_zero_summary_is_not_the_reason() {
    // Tests passed, a later step failed: the old description was `ℹ fail 0`.
    let log = "ℹ tests 10\nℹ pass 10\nℹ fail 0\n+ 03:59:01 npm run build\nsh: 1: tsc: not found\n";
    let f = Failure::of(Step::Check, log);
    assert_eq!(f.summary, "npm run build: sh: 1: tsc: not found");
    assert!(!f.summary.contains("fail 0"));
    let quiet = "ℹ fail 0\n+ 03:59:01 npm run lint\n";
    let f = Failure::of(Step::Check, quiet);
    assert_eq!(f.summary, "npm run lint: exited with an error");
    assert_eq!(f.step, "npm run lint");
}

#[test]
fn failed_tests_are_counted() {
    assert_eq!(
        Failure::of(Step::Check, "ℹ pass 7\nℹ fail 3\n").summary,
        "test: 3 failed"
    );
    assert_eq!(Failure::of(Step::Check, "# fail 2\n").summary, "test: 2 failed");
}

#[test]
fn a_failed_fetch_and_a_timeout_are_named() {
    assert_eq!(
        Failure::of(Step::Fetch, "fatal: bad ref\n").summary,
        "fetch failed: fatal: bad ref"
    );
    let f = Failure::of(Step::Check, "x\nerror: timed out\n");
    assert_eq!((f.step.as_str(), f.summary.as_str()), ("timed out", "timed out"));
}

#[test]
fn the_excerpt_is_never_empty_and_fits_the_store() {
    assert_eq!(Failure::of(Step::Check, "").excerpt, "(the run's log was empty)");
    let long = "é𝄞x".repeat(3000);
    let e = excerpt_of(&long);
    assert!(e.encode_utf16().count() <= EXCERPT_MAX);
    assert!(long.ends_with(&e));
}
