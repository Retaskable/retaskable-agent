//! Filling `prompts/reviewer.md` for the stage 3 session (06.3-review.md, "Stage 3: final
//! review"). The code-host abstraction (`{{change}}`, `hostVars`) isn't ported here, matching
//! `scouts::prompt`: the template hardcodes "PR", as the rest of this agent's prompts do.
use anyhow::Result;

use crate::release_notes::render_prompt;

/// The reviewer session's prompt vars besides the scouts' own findings text.
pub struct ReviewerContext<'a> {
    pub item_id: &'a str,
    pub title: &'a str,
    pub slice: u32,
    /// Joined with ", ", or "(none)" when the item links no specs.
    pub spec_refs: &'a str,
    pub slice_description: &'a str,
    /// The range the reviewer reads with `git diff {{range}} -- <file>` to confirm a finding.
    pub range: &'a str,
    /// The re-review note: this review round and the earlier requests to check, or "" for a
    /// first review.
    pub scope: &'a str,
    /// Stage 1's check results and diff stat, the same evidence a scout reads.
    pub checks: &'a str,
    /// Every scout's findings, already rendered by `scouts::format_scout` and joined.
    pub scouts: &'a str,
    /// The User Manual versions the product owner has signed off, as "<version>, approved
    /// <date> by <login>" lines, or "(none)".
    pub manual_approvals: &'a str,
}

/// Fills `prompts/reviewer.md`'s template with `ctx`.
pub fn build_prompt(template: &str, ctx: &ReviewerContext<'_>) -> Result<String> {
    render_prompt(
        template,
        &[
            ("itemId", ctx.item_id.to_string()),
            ("title", ctx.title.to_string()),
            ("slice", ctx.slice.to_string()),
            ("specRefs", ctx.spec_refs.to_string()),
            ("sliceDescription", ctx.slice_description.to_string()),
            ("range", ctx.range.to_string()),
            ("scope", ctx.scope.to_string()),
            ("checks", ctx.checks.to_string()),
            ("scouts", ctx.scouts.to_string()),
            ("manualApprovals", ctx.manual_approvals.to_string()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn template() -> String {
        std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("reviewer.md"),
        )
        .unwrap()
    }

    fn ctx<'a>() -> ReviewerContext<'a> {
        ReviewerContext {
            item_id: "my-item",
            title: "My item",
            slice: 2,
            spec_refs: "docs/specs/agent/06.3-review.md",
            slice_description: "Ports the stage 3 reviewer.",
            range: "origin/main...HEAD",
            scope: "",
            checks: "✓ test",
            scouts: "### correctness\n(no findings)",
            manual_approvals: "(none)",
        }
    }

    #[test]
    fn build_prompt_fills_the_reviewer_template() {
        let prompt = build_prompt(&template(), &ctx()).unwrap();
        assert!(!prompt.contains("{{"), "{prompt}");
        assert!(prompt.contains("my-item"));
        assert!(prompt.contains("Ports the stage 3 reviewer."));
        assert!(prompt.contains("### correctness"));
        assert!(prompt.contains("(none)"));
    }

    #[test]
    fn build_prompt_carries_the_re_review_note_when_given() {
        let mut ctx = ctx();
        ctx.scope = "This is review round 2. Earlier requests: fix the off-by-one in src/x.rs.";
        let prompt = build_prompt(&template(), &ctx).unwrap();
        assert!(prompt.contains("review round 2"));
    }

    #[test]
    fn build_prompt_fails_on_a_template_with_a_placeholder_it_cant_fill() {
        let err = build_prompt("{{unknown}}", &ctx()).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }
}
