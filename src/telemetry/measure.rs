//! What the measurement events share (agent-telemetry.md section 3.4): a bounded text, a run's short name, and the
//! reason code a person-needed stop is grouped by. Ids, codes and short texts only: never a prompt or a secret.
use std::hash::{Hash, Hasher};

use super::{MAX_TEXT, scrub};

/// `text` fit for the log: secrets replaced, one line, at most [`MAX_TEXT`] characters.
pub fn clip(text: &str) -> String {
    scrub(text, MAX_TEXT)
}

/// A run's name in the log: twelve hex digits of its token's hash, the same in every event of the run, and not the
/// token (which the store accepts as proof of the run).
pub fn run_ref(token: &str) -> String {
    // `DefaultHasher::new()` has fixed keys, so one token gives one name on every start.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut h);
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

/// A short code for why something needs a person, from the words the agent wrote for it, so stops can be counted by
/// cause. The words themselves ride along in the event.
pub fn reason_code(text: &str) -> &'static str {
    let t = text.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if has(&[
        "backlog didn't take",
        "store didn't take",
        "store refused",
        "shared backlog refused",
    ]) {
        "store_refused"
    } else if has(&[
        "no commits between",
        "made no commit",
        "no commit",
        "nothing was committed",
    ]) {
        "no_commit"
    } else if has(&[
        "not found",
        "not installed",
        "no such file or directory",
        "is not recognized",
    ]) && has(&[
        "command",
        "program",
        "cargo",
        "node",
        "npm",
        "git",
        "gh",
        ": not found",
        "not installed",
    ]) {
        "missing_program"
    } else if t.contains("push")
        && has(&[
            "reject",
            "refused",
            "failed",
            "couldn't be published",
            "non-fast-forward",
        ])
    {
        "push_rejected"
    } else if has(&["nothing left", "nothing to build", "nothing more to build"]) {
        "nothing_left"
    } else if has(&["only steps a person can do", "person_steps", "person steps"]) {
        "person_steps"
    } else if has(&["question", "clarif"]) {
        "questions"
    } else if has(&[
        "ci timed out",
        "ci kept failing",
        "ci failed",
        "ci is red",
        "failing ci",
        "ci red",
    ]) {
        "ci_red"
    } else if has(&["checks kept failing", "checks failed"]) {
        "checks_red"
    } else if has(&["review escalated", "review rounds", "review's rounds"]) {
        "review_escalated"
    } else if has(&["conflict"]) {
        "merge_conflict"
    } else if has(&[
        "merge failed",
        "merge step",
        "couldn't merge",
        "failed to merge",
        "merge",
    ]) && t.contains("fail")
    {
        "merge_failed"
    } else if has(&["closed without merging"]) {
        "pr_closed"
    } else if has(&["usage limit", "rate limit", "limit"]) {
        "limit"
    } else if has(&["gave up"]) {
        "gave_up"
    } else if has(&[
        "couldn't resume",
        "crashed",
        "ended without answering",
        "didn't start",
        "couldn't start",
    ]) {
        "session_failed"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stops_seen_in_the_field_get_their_codes() {
        for (text, code) in [
            (
                "pull request create failed: No commits between main and branch",
                "no_commit",
            ),
            (
                "The work is committed but couldn't be published: git push rejected",
                "push_rejected",
            ),
            ("the checks ran `cargo`: cargo: not found", "missing_program"),
            ("CI timed out or kept failing, past the allowed rounds.", "ci_red"),
            ("The checks kept failing, past the allowed rounds.", "checks_red"),
            ("The implementer has questions for you", "questions"),
            ("Nothing left to build: it's done", "nothing_left"),
            (
                "Nothing left to build, but the shared backlog didn't take it (409)",
                "store_refused",
            ),
            ("Only steps a person can do are left:\n1. deploy", "person_steps"),
            ("The review escalated, or its rounds ran out.", "review_escalated"),
            ("The merge failed, or its conflict rounds ran out.", "merge_conflict"),
            ("The pull request was closed without merging.", "pr_closed"),
            ("something unforeseen", "other"),
        ] {
            assert_eq!(reason_code(text), code, "{text}");
        }
    }

    #[test]
    fn a_run_ref_is_stable_short_and_not_the_token() {
        let a = run_ref("tok-123");
        assert_eq!(a, run_ref("tok-123"));
        assert_ne!(a, run_ref("tok-124"));
        assert_eq!(a.len(), 12);
        assert!(!a.contains("tok"));
    }

    #[test]
    fn a_text_is_cut_and_stripped_of_secrets() {
        let long = "x".repeat(1000);
        assert!(clip(&long).chars().count() <= MAX_TEXT);
        assert!(!clip("token=ghp_abcdefghijklmnopqrstuvwxyz0123456789").contains("ghp_"));
    }
}
