//! Filling `prompts/scout.md` for one scout job (the scout half of `prompt("scout", ...)` in
//! Retaskable.com's `agent/src/review.ts`). The code-host abstraction (`{{change}}`, `hostVars`)
//! isn't ported here: the template hardcodes "PR", as the rest of this agent's prompts do, and
//! `backlogFileLine` (always empty for a hosted backlog: no backlog file in this repo's
//! worktrees) is left out of the template rather than carried as an always-empty variable.
use anyhow::Result;

use crate::release_notes::render_prompt;
use crate::review_context::ReviewContext;

use super::info::ScoutInfo;
use super::plan::ScoutJob;

/// Fills `prompts/scout.md`'s template with `ctx`, `job`'s files, and `info`'s focus and
/// instructions.
pub fn build_prompt(template: &str, ctx: &ReviewContext<'_>, job: &ScoutJob, info: &ScoutInfo) -> Result<String> {
    let files = if job.files.is_empty() {
        "(no files)".to_string()
    } else {
        job.files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut vars = ctx.vars();
    vars.push(("files", files));
    vars.push(("focus", info.focus.clone()));
    vars.push(("instructions", info.instructions.clone()));
    render_prompt(template, &vars)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::plan::ScoutKind;
    use super::*;

    fn template() -> String {
        std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("scout.md"),
        )
        .unwrap()
    }

    fn ctx<'a>() -> ReviewContext<'a> {
        ReviewContext {
            item_id: "my-item",
            title: "My item",
            slice: 2,
            spec_refs: "docs/specs/agent/06.3-review.md",
            slice_description: "Ports scout planning.",
            range: "origin/main...HEAD",
            scope: "",
            checks: "✓ test",
        }
    }

    fn info() -> ScoutInfo {
        ScoutInfo {
            focus: "correctness".to_string(),
            instructions: "look for bugs".to_string(),
        }
    }

    #[test]
    fn build_prompt_fills_the_scout_template() {
        let job = ScoutJob {
            scout: ScoutKind::Correctness,
            label: "correctness".to_string(),
            files: vec!["src/x.rs".to_string(), "src/y.rs".to_string()],
        };
        let prompt = build_prompt(&template(), &ctx(), &job, &info()).unwrap();
        assert!(!prompt.contains("{{"), "{prompt}");
        assert!(prompt.contains("one PR"));
        assert!(prompt.contains("my-item"));
        assert!(prompt.contains("- src/x.rs"));
        assert!(prompt.contains("- src/y.rs"));
        assert!(prompt.contains("look for bugs"));
    }

    #[test]
    fn build_prompt_lists_no_files_when_the_job_has_none() {
        let job = ScoutJob {
            scout: ScoutKind::Tests,
            label: "tests".to_string(),
            files: vec![],
        };
        let prompt = build_prompt(&template(), &ctx(), &job, &info()).unwrap();
        assert!(prompt.contains("(no files)"));
    }

    #[test]
    fn build_prompt_fails_on_a_template_with_a_placeholder_it_cant_fill() {
        let job = ScoutJob {
            scout: ScoutKind::Spec,
            label: "spec".to_string(),
            files: vec![],
        };
        let err = build_prompt("{{unknown}}", &ctx(), &job, &info()).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }
}
