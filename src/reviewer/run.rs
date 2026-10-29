//! Building the stage 3 session and rendering its verdict for the PR (06.3-review.md, "Stage 3:
//! final review"): a single read-only, ephemeral session, run through the caller's
//! [`crate::engines::runner::run_session`], then [`format_review`]'s comment text for
//! `gh pr comment` (a separate, not-yet-built orchestrator-loop concern: posting it, and turning
//! `verdict` into the item's next state, both come with that wiring).
use anyhow::Result;

use crate::engines::{EphemeralSessionConfig, SessionSpec};

use super::model::{ReviewerResult, reviewer_result_schema};
use super::prompt::{ReviewerContext, build_prompt};

/// What the reviewer session runs with, besides its [`ReviewerContext`].
pub struct Reviewer<'a> {
    /// `prompts/reviewer.md`.
    pub template: &'a str,
    /// `Role::Reviewer`'s engine/model/budget/local/cursor/sandbox bundle; `session.cwd` is a
    /// checkout at the slice's head, where the reviewer reads cited lines to confirm a finding.
    pub session: EphemeralSessionConfig<'a>,
}

/// The read-only, ephemeral session for the stage 3 review.
pub fn session_spec(w: &Reviewer<'_>, ctx: &ReviewerContext<'_>) -> Result<SessionSpec> {
    let prompt = build_prompt(w.template, ctx)?;
    Ok(w.session.spec(prompt, reviewer_result_schema()))
}

/// The PR comment the orchestrator posts once this session finishes (`gh pr comment`, since
/// GitHub won't let a PR's own author approve it): the check results, the confirmed findings
/// (the reviewer's own `comments`) and the dismissed ones, so the reasoning is on record.
pub fn format_review(checks: &str, result: &ReviewerResult) -> String {
    let mut out = format!("**Verdict:** {}\n\n{}", result.verdict, result.summary);

    out.push_str("\n\n**Checks**\n");
    out.push_str(if checks.is_empty() {
        "(no checks configured)"
    } else {
        checks
    });

    out.push_str("\n\n**Findings**\n");
    out.push_str(&if result.comments.is_empty() {
        "(none)".to_string()
    } else {
        result
            .comments
            .iter()
            .map(|c| match (&c.file, c.line) {
                (Some(file), Some(line)) => format!("- {file}:{line} — {}", c.body),
                (Some(file), None) => format!("- {file} — {}", c.body),
                _ => format!("- {}", c.body),
            })
            .collect::<Vec<_>>()
            .join("\n")
    });

    out.push_str("\n\n**Dismissed**\n");
    out.push_str(&if result.dismissed_findings.is_empty() {
        "(none)".to_string()
    } else {
        result
            .dismissed_findings
            .iter()
            .map(|d| format!("- [{}] {}:{} — {}", d.scout, d.file, d.line, d.reason))
            .collect::<Vec<_>>()
            .join("\n")
    });

    if !result.person_steps.is_empty() {
        out.push_str("\n\n**Steps needed before this merges**\n");
        out.push_str(
            &result
                .person_steps
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::reviewer::model::{DismissedFinding, ReviewerComment};

    fn writer(cwd: &'static Path) -> Reviewer<'static> {
        Reviewer {
            template: "{{itemId}} {{title}} {{slice}} {{specRefs}} {{sliceDescription}} {{range}} {{scope}} {{checks}} {{scouts}} {{manualApprovals}}",
            session: EphemeralSessionConfig {
                cwd,
                budget_usd: 2.0,
                model: "opus".to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
            },
        }
    }

    fn ctx<'a>() -> ReviewerContext<'a> {
        ReviewerContext {
            item_id: "my-item",
            title: "My item",
            slice: 1,
            spec_refs: "(none)",
            slice_description: "(none given)",
            range: "origin/main...HEAD",
            scope: "",
            checks: "✓ test",
            scouts: "### correctness\n(no findings)",
            manual_approvals: "(none)",
        }
    }

    #[test]
    fn session_spec_fills_the_template_as_a_readonly_ephemeral_session() {
        let cwd = Path::new("repo");
        let spec = session_spec(&writer(cwd), &ctx()).unwrap();
        assert!(spec.readonly);
        assert_eq!(spec.model, "opus");
        assert_eq!(spec.schema, reviewer_result_schema());
        assert!(spec.prompt.contains("my-item"));
    }

    #[test]
    fn session_spec_fails_when_the_template_has_an_unknown_placeholder() {
        let cwd = Path::new("repo");
        let mut w = writer(cwd);
        w.template = "{{unknown}}";
        assert!(session_spec(&w, &ctx()).is_err());
    }

    fn result(verdict: &str) -> ReviewerResult {
        serde_json::from_value(json!({ "verdict": verdict, "summary": "Looks right." })).unwrap()
    }

    #[test]
    fn format_review_shows_no_findings_or_dismissals_when_there_are_none() {
        let text = format_review("✓ test", &result("approve"));
        assert!(text.starts_with("**Verdict:** approve\n\nLooks right."));
        assert!(text.contains("**Checks**\n✓ test"));
        assert!(text.contains("**Findings**\n(none)"));
        assert!(text.contains("**Dismissed**\n(none)"));
        assert!(!text.contains("Steps needed"));
    }

    #[test]
    fn format_review_renders_comments_with_and_without_a_file_or_line() {
        let mut r = result("request_changes");
        r.comments = vec![
            ReviewerComment {
                file: Some("src/x.rs".to_string()),
                line: Some(10),
                body: "off by one".to_string(),
            },
            ReviewerComment {
                file: None,
                line: None,
                body: "general note".to_string(),
            },
        ];
        let text = format_review("✓ test", &r);
        assert!(text.contains("- src/x.rs:10 — off by one"));
        assert!(text.contains("- general note"));
    }

    #[test]
    fn format_review_renders_dismissed_findings_and_person_steps() {
        let mut r = result("person_steps");
        r.dismissed_findings = vec![DismissedFinding {
            scout: "tests".to_string(),
            file: "src/x.rs".to_string(),
            line: 20,
            reason: "already covered".to_string(),
        }];
        r.person_steps = vec!["Add the API key in production.".to_string()];
        let text = format_review("✓ test", &r);
        assert!(text.contains("- [tests] src/x.rs:20 — already covered"));
        assert!(text.contains("**Steps needed before this merges**\n- Add the API key in production."));
    }

    #[test]
    fn format_review_says_no_checks_configured_when_checks_is_empty() {
        let text = format_review("", &result("approve"));
        assert!(text.contains("**Checks**\n(no checks configured)"));
    }
}
