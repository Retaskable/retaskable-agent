//! A failed plan always says why (docs/specs/plans-say-why.md steps 1 and 2): the reason a failure is reported with, and
//! the words a retry's drafter is given about the last failure.
use serde_json::Value;

use crate::backlog::drafts::ClaimedDraft;
use crate::text::{collapse_and_clip, non_blank};

/// Said when a failure arrives with no words at all.
pub const NO_REASON: &str = "The plan failed and no reason was recorded.";
/// What the store's summary starts with after a failed first try, before the reason.
pub const RETRY_PREFIX: &str = "Trying again by itself: ";

/// The reason to report: `raw` trimmed, or `fallback` (the caller's plain words, built from what it knows) when it's
/// blank.
pub fn reason_or(raw: &str, fallback: &str) -> String {
    non_blank(raw).unwrap_or(fallback).trim().to_string()
}

/// A drafter that failed with an error: its message, or a plain line naming what kind of failure it was.
pub fn error_reason(message: &str, kind: &str) -> String {
    reason_or(
        message,
        &format!("Writing the plan stopped with an error that had no message ({kind})."),
    )
}

/// The reason the draft last failed, from what the store handed out: its `error`, else the summary a first failure left
/// (after [`RETRY_PREFIX`]).
pub fn failure_reason(d: &ClaimedDraft) -> Option<String> {
    let text = |k: &str| d.other.get(k).and_then(Value::as_str).and_then(non_blank);
    text("error")
        .or_else(|| text("summary").and_then(|s| s.strip_prefix(RETRY_PREFIX)))
        .and_then(non_blank)
        .map(str::to_string)
}

/// What a drafter started again after a failure is told, appended to its prompt.
pub fn retry_text(reason: Option<&str>) -> String {
    let told = match reason {
        Some(r) => format!("It failed with this reason: {}", collapse_and_clip(r, 300)),
        None => "No reason was recorded for the failure.".to_string(),
    };
    format!(
        "\n\n## This is a retry\n\nEarlier work on this request failed, and the branch holds what it left. {told}\n\nCarry on from the branch. If you still cannot write the plan, return `failed` with the exact reason."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(extra: Value) -> ClaimedDraft {
        let mut v = serde_json::json!({ "id": "d1" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_blank_reason_becomes_the_fallback() {
        assert_eq!(reason_or("  \n", NO_REASON), NO_REASON);
        assert_eq!(reason_or(" boom ", NO_REASON), "boom");
        assert_eq!(
            error_reason("", "spawn"),
            "Writing the plan stopped with an error that had no message (spawn)."
        );
    }

    #[test]
    fn the_reason_is_the_error_or_the_first_failures_summary() {
        assert_eq!(
            failure_reason(&draft(serde_json::json!({ "error": "x" }))).as_deref(),
            Some("x")
        );
        let d = draft(serde_json::json!({ "summary": "Trying again by itself: y" }));
        assert_eq!(failure_reason(&d).as_deref(), Some("y"));
        assert_eq!(
            failure_reason(&draft(serde_json::json!({ "summary": "Plan written" }))),
            None
        );
    }

    #[test]
    fn the_retry_text_gives_the_reason_or_says_none_was_recorded() {
        assert!(retry_text(Some("it broke")).contains("with this reason: it broke"));
        let none = retry_text(None);
        assert!(none.contains("No reason was recorded") && none.contains("exact reason"));
    }
}
