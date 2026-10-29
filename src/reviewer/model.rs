//! The final reviewer session's structured answer (`REVIEWER_RESULT_SCHEMA`/the reviewer's result
//! shape, 06.3-review.md "Stage 3: final review").
use serde::Deserialize;
use serde_json::{Value, json};

use crate::implementer::Playtest;

/// One problem the reviewer is noting on the PR (`comments`): the confirmed findings, or anything
/// else worth knowing on an `approve`. `file`/`line` are optional — a comment can be general.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReviewerComment {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
    pub body: String,
}

/// One scout finding the reviewer threw out (`dismissedFindings`), kept on the PR comment so the
/// reasoning is on record.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DismissedFinding {
    pub scout: String,
    pub file: String,
    pub line: i64,
    pub reason: String,
}

/// The reviewer's full answer. `verdict` stays a `String` (not an enum), the same reason
/// `ImplementerResult::status` does: an unrecognized value a future prompt adds shouldn't fail to
/// deserialize, and callers match on it the way the orchestrator's step function does.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewerResult {
    pub verdict: String,
    #[serde(default, rename = "personSteps")]
    pub person_steps: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub comments: Vec<ReviewerComment>,
    #[serde(default, rename = "dismissedFindings")]
    pub dismissed_findings: Vec<DismissedFinding>,
    #[serde(default)]
    pub playtest: Option<Playtest>,
}

/// `REVIEWER_RESULT_SCHEMA` (06.3-review.md), for the reviewer session's `schema`.
pub fn reviewer_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "enum": ["approve", "request_changes", "needs_human", "person_steps"] },
            "personSteps": { "type": "array", "items": { "type": "string" } },
            "summary": { "type": "string" },
            "comments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string" },
                        "line": { "type": "number" },
                        "body": { "type": "string" }
                    },
                    "required": ["body"]
                }
            },
            "dismissedFindings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "scout": { "type": "string" },
                        "file": { "type": "string" },
                        "line": { "type": "number" },
                        "reason": { "type": "string" }
                    },
                    "required": ["scout", "file", "line", "reason"]
                }
            },
            "playtest": {
                "type": "object",
                "properties": {
                    "what": { "type": "string" },
                    "steps": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["what", "steps"]
            }
        },
        "required": ["verdict", "summary"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewer_result_parses_a_minimal_approval() {
        let r: ReviewerResult = serde_json::from_value(json!({
            "verdict": "approve",
            "summary": "Looks right.",
        }))
        .unwrap();
        assert_eq!(r.verdict, "approve");
        assert!(r.person_steps.is_empty());
        assert!(r.comments.is_empty());
        assert!(r.dismissed_findings.is_empty());
        assert!(r.playtest.is_none());
    }

    #[test]
    fn reviewer_result_parses_every_field() {
        let r: ReviewerResult = serde_json::from_value(json!({
            "verdict": "request_changes",
            "personSteps": [],
            "summary": "One bug left.",
            "comments": [{ "file": "src/x.rs", "line": 10, "body": "off by one" }],
            "dismissedFindings": [{
                "scout": "tests",
                "file": "src/x.rs",
                "line": 20,
                "reason": "already covered by an existing test",
            }],
            "playtest": { "what": "Open the page.", "steps": ["Press Save."] },
        }))
        .unwrap();
        assert_eq!(r.comments[0].file.as_deref(), Some("src/x.rs"));
        assert_eq!(r.comments[0].line, Some(10));
        assert_eq!(r.dismissed_findings[0].scout, "tests");
        assert_eq!(r.playtest.unwrap().steps, vec!["Press Save."]);
    }

    #[test]
    fn reviewer_result_allows_a_comment_with_no_file_or_line() {
        let r: ReviewerResult = serde_json::from_value(json!({
            "verdict": "approve",
            "summary": "Fine overall.",
            "comments": [{ "body": "nice use of the existing helper" }],
        }))
        .unwrap();
        assert_eq!(r.comments[0].file, None);
        assert_eq!(r.comments[0].line, None);
    }

    #[test]
    fn schema_requires_verdict_and_summary_and_lists_the_verdict_enum() {
        let schema = reviewer_result_schema();
        assert_eq!(schema["required"], json!(["verdict", "summary"]));
        assert_eq!(
            schema["properties"]["verdict"]["enum"],
            json!(["approve", "request_changes", "needs_human", "person_steps"])
        );
    }
}
