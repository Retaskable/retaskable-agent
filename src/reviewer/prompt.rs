//! Filling `prompts/reviewer.md` for the stage 3 session (06.3-review.md, "Stage 3: final
//! review"). The code-host abstraction (`{{change}}`, `hostVars`) isn't ported here, matching
//! `scouts::prompt`: the template hardcodes "PR", as the rest of this agent's prompts do.
use anyhow::Result;

use crate::implementer::Playtest;
use crate::prompt::render_prompt;
use crate::review_context::ReviewContext;

/// The reviewer session's prompt vars: the item, diff range and stage 1 evidence it shares with
/// a scout's own prompt ([`ReviewContext`]), plus the diff stat, the scouts' findings, the
/// implementer's suggested playtest and the manual approvals only the final review reads.
pub struct ReviewerContext<'a> {
    pub common: ReviewContext<'a>,
    /// `git diff --stat <range>`, from the stage 1 results.
    pub diff_stat: &'a str,
    /// Every scout's findings, already rendered by `scouts::format_scout` and joined.
    pub scouts: &'a str,
    /// The implementer's suggested playtest, or `None` if it didn't suggest one.
    pub playtest: Option<&'a Playtest>,
    /// The User Manual versions the product owner has signed off, as "<version>, approved
    /// <date> by <login>" lines, or "(none)".
    pub manual_approvals: &'a str,
}

/// Fills `prompts/reviewer.md`'s template with `ctx`.
pub fn build_prompt(template: &str, ctx: &ReviewerContext<'_>) -> Result<String> {
    let mut vars = ctx.common.vars();
    vars.push(("diffStat", ctx.diff_stat.to_string()));
    vars.push(("scouts", ctx.scouts.to_string()));
    vars.push(("playtest", format_playtest(ctx.playtest)));
    vars.push(("manualApprovals", ctx.manual_approvals.to_string()));
    render_prompt(template, &vars)
}

/// The implementer's suggested playtest as the reviewer reads it: `what`, then each step, or
/// "(none suggested)" if the implementer didn't suggest one.
fn format_playtest(playtest: Option<&Playtest>) -> String {
    let Some(playtest) = playtest else {
        return "(none suggested)".to_string();
    };
    if playtest.steps.is_empty() {
        return playtest.what.clone();
    }
    let steps = playtest
        .steps
        .iter()
        .map(|s| format!("- {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n{steps}", playtest.what)
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
            common: ReviewContext {
                item_id: "my-item",
                title: "My item",
                slice: 2,
                spec_refs: "docs/specs/agent/06.3-review.md",
                slice_description: "Ports the stage 3 reviewer.",
                range: "origin/main...HEAD",
                scope: "",
                checks: "✓ test",
            },
            diff_stat: "1 file changed, 10 insertions(+)",
            scouts: "### correctness\n(no findings)",
            playtest: None,
            manual_approvals: "(none)",
        }
    }

    #[test]
    fn build_prompt_fills_the_reviewer_template() {
        let prompt = build_prompt(&template(), &ctx()).unwrap();
        assert!(!prompt.contains("{{"), "{prompt}");
        assert!(prompt.contains("my-item"));
        assert!(prompt.contains("Ports the stage 3 reviewer."));
        assert!(prompt.contains("1 file changed, 10 insertions(+)"));
        assert!(prompt.contains("### correctness"));
        assert!(prompt.contains("(none suggested)"));
        assert!(prompt.contains("(none)"));
    }

    #[test]
    fn build_prompt_shows_the_implementers_suggested_playtest() {
        let playtest = Playtest {
            what: "Check the new button appears.".to_string(),
            steps: vec!["Open the settings page.".to_string(), "Click the button.".to_string()],
        };
        let mut ctx = ctx();
        ctx.playtest = Some(&playtest);
        let prompt = build_prompt(&template(), &ctx).unwrap();
        assert!(prompt.contains("Check the new button appears."));
        assert!(prompt.contains("- Open the settings page."));
        assert!(prompt.contains("- Click the button."));
    }

    #[test]
    fn build_prompt_carries_the_re_review_note_when_given() {
        let mut ctx = ctx();
        ctx.common.scope = "This is review round 2. Earlier requests: fix the off-by-one in src/x.rs.";
        let prompt = build_prompt(&template(), &ctx).unwrap();
        assert!(prompt.contains("review round 2"));
    }

    #[test]
    fn build_prompt_fails_on_a_template_with_a_placeholder_it_cant_fill() {
        let err = build_prompt("{{unknown}}", &ctx()).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }
}
