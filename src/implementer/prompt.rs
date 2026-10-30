//! Fills `prompts/implementer.md` (agent/src/prompts.ts's `backlogVars`/`projectVars`/`hostVars`,
//! and `checksText`/`gateRule` in `agent/src/steps.ts`/`codeGates.ts`, combined into one builder for
//! this agent's own [`super::model::ImplementerResult`]/[`super::model::implementer_result_schema`]).
//! This agent has no code-host abstraction yet ([`agent.md`] section 3.4 names only GitHub's `gh`),
//! so `{{pr}}`/`{{change}}` are hardcoded to GitHub's words rather than built from a config.
use anyhow::Result;

use crate::backlog::Item;
use crate::config::{CheckConfig, CodeGates, ProjectType};
use crate::prompt::{project_words, render_prompt};

/// `checksText` (agent/src/steps.ts): the prompt's bulleted list of a project's checks, one per
/// line, or a line saying there are none.
pub fn checks_text(checks: &[CheckConfig]) -> String {
    if checks.is_empty() {
        return "   - (none configured)".to_string();
    }
    checks
        .iter()
        .map(|c| {
            format!(
                "   - `{}`{}",
                c.command,
                if c.blocking { "" } else { " (non-blocking)" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `gateRule` (agent/src/codeGates.ts): the prompts' sentence saying what the code gates check
/// (section 6.2's "Code gates before the push"), empty when every gate is off, so a session
/// doesn't spend a round finding out.
pub fn gate_rule(cfg: &CodeGates) -> String {
    let asks: Vec<String> = [
        (cfg.max_file_lines > 0).then(|| {
            format!(
                "keep every source file you touch at {} lines or fewer (split one that's already over)",
                cfg.max_file_lines
            )
        }),
        (cfg.dup_lines > 0).then(|| "reuse the project's existing code rather than copying it".to_string()),
        (cfg.max_spec_chars > 0).then(|| {
            format!(
                "keep every spec you touch under {} characters (split one that's over into a section folder with an index)",
                cfg.max_spec_chars
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    if asks.is_empty() {
        return String::new();
    }
    let which = if asks.len() > 2 {
        "these"
    } else if asks.len() > 1 {
        "both"
    } else {
        "this"
    };
    format!(
        " {}: Retaskable checks {which} before it pushes your branch, and sends the work back if not.",
        capitalize(&asks.join(", and "))
    )
}

/// `backlogVars` (agent/src/prompts.ts): the implementer prompt's fixed words about the shared
/// backlog, with `{{pr}}`/`{{change}}` already filled to GitHub's wording (`hostVars`, hardcoded —
/// see the module doc comment).
const SOURCE_OF_TRUTH: &str = "The specs are the source of truth for what is built. Retaskable keeps the backlog itself (its order and each item's progress), so no file in the repo holds it.";
const REMAINING_LABEL: &str = "What the last slice said is still to do";
const ITEM_ENTRY: &str = "the item above (its title, what's left of it and its spec links)";
const ENTRY_SHORT: &str = "the item";
const NOTHING_LEFT: &str = "and you return `itemDone: true`";
const NOTHING_TO_CHANGE: &str = " If there isn't even a spec to update (the specs on the base branch already describe the whole item as built, or name the later item that now has the rest of it), open no PR and commit nothing new: return `status: \"nothing_left\"`, with `summary` saying where it's built or which item has the rest. Retaskable then closes any pull request still open for this item, unmerged, and marks the item built. Setup only a person can do isn't nothing left (rule 4's done): that's `failed`, saying what they have to do. Don't ask a question whose answer the specs already give. When all of the item's code is on the base branch and only steps a person can do are left (an account, a secret, a setting in production, an invitation), open no PR: return `status: \"person_steps\"` with `personSteps`, each step one short sentence a person can follow, in order. Retaskable then waits for them, and a person finishes the item once the steps are done. If a person's note says they've done the steps, believe them: say so in the spec, and finish the item. When the item can't be built until **another item in the backlog** is built first (it needs code or infrastructure that item brings, which isn't on the base branch yet, so building it here would mean doing that item's work), don't build it: open no PR and return `status: \"waits_on\"` with `waitsOn`, the ids of those items (from **Items not built yet** below), and `summary` saying what this item needs from each. Retaskable makes the item wait for them and puts it back in the queue until they're built. Only for an item on that list, never for work this item could do itself.";
const DONE_MEANS: &str = "**Done means someone can use it now**, not that the code for the item's words exists: if it has no way in yet (e.g. an endpoint with no button or page that reaches it) or it needs setup only a human can do (an account, an OAuth app, secrets, DNS)";
const DONE_RETURN: &str = "`itemDone` and `remaining` (rule 4), ";

fn backlog_rule() -> String {
    format!(
        "**Say whether the item is done; don't edit a backlog file.** Update any spec the slice changes to describe what is now built, in its own \"as built\" style, but there's no backlog file to change: Retaskable keeps the backlog. Return `itemDone: true` only if nothing in the item is left once this slice merges. Otherwise return `itemDone: false` and `remaining`: what's still to do, in a sentence or two. {DONE_MEANS}, it isn't done, and `remaining` says exactly what's left, including that setup. The exception: when a **later item in the backlog** is the one that brings the way in (for example \"the demo organisation and its route are the demo-org item\"), the item is done and the spec names that item, so the items that depend on it can start."
    )
}

/// What a fresh implementer session's prompt is built from, besides the item and project config:
/// the "Where to start" text ([`super::start_files::render`], or empty for a session that needs
/// none yet), the branch it's working on, and the repo it's in.
pub struct Prompt<'a> {
    pub item: &'a Item,
    pub project_type: ProjectType,
    pub branch: &'a str,
    pub base_branch: &'a str,
    pub github_repo: &'a str,
    pub checks: &'a [CheckConfig],
    pub code_gates: CodeGates,
    /// [`super::start_files::render`]'s output, appended after the rules; empty for none.
    pub where_to_start: &'a str,
    /// `{{choreKind}}`/`{{choreInstructions}}` (`prompts/chore.md`): empty for a roadmap item's own
    /// template, which never references them.
    pub chore_kind: &'a str,
    pub chore_instructions: &'a str,
}

/// Fills `prompts/implementer.md` for a fresh session on `p.item`.
pub fn build_prompt(template: &str, p: &Prompt<'_>) -> Result<String> {
    let words = project_words(p.project_type);
    let filled = render_prompt(
        template,
        &[
            ("pr", "PR".to_string()),
            ("change", "pull request".to_string()),
            ("sourceOfTruth", SOURCE_OF_TRUTH.to_string()),
            ("itemId", p.item.id.clone()),
            ("title", p.item.title.clone()),
            ("slice", p.item.slice.to_string()),
            ("remainingLabel", REMAINING_LABEL.to_string()),
            (
                "remaining",
                p.item
                    .remaining
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("(nothing listed)")
                    .to_string(),
            ),
            (
                "specRefs",
                if p.item.spec_refs.is_empty() {
                    "(none)".to_string()
                } else {
                    p.item.spec_refs.join(", ")
                },
            ),
            ("backlogLine", String::new()),
            ("branch", p.branch.to_string()),
            ("baseBranch", p.base_branch.to_string()),
            ("githubRepo", p.github_repo.to_string()),
            ("itemEntry", ITEM_ENTRY.to_string()),
            ("entryShort", ENTRY_SHORT.to_string()),
            ("nothingLeft", NOTHING_LEFT.to_string()),
            ("nothingToChange", NOTHING_TO_CHANGE.to_string()),
            ("checks", checks_text(p.checks)),
            ("codeLimits", gate_rule(&p.code_gates)),
            ("backlogRule", backlog_rule()),
            ("doneReturn", DONE_RETURN.to_string()),
            ("projectNoun", words.noun.to_string()),
            ("projectUser", words.user.to_string()),
            ("projectExamples", words.examples.to_string()),
            ("projectSteps", words.steps.to_string()),
            ("choreKind", p.chore_kind.to_string()),
            ("choreInstructions", p.chore_instructions.to_string()),
        ],
    )?;
    if p.where_to_start.is_empty() {
        Ok(filled)
    } else {
        Ok(format!("{filled}\n\n{}", p.where_to_start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementer::test_support::{checks, item};

    #[test]
    fn checks_text_lists_each_check_and_marks_non_blocking() {
        assert_eq!(
            checks_text(&checks()),
            "   - `cargo test`\n   - `cargo clippy` (non-blocking)"
        );
        assert_eq!(checks_text(&[]), "   - (none configured)");
    }

    #[test]
    fn gate_rule_matches_the_ported_wording_for_each_combination() {
        assert_eq!(
            gate_rule(&CodeGates {
                max_file_lines: 1000,
                dup_lines: 8,
                max_spec_chars: 0
            }),
            " Keep every source file you touch at 1000 lines or fewer (split one that's already over), and reuse the project's existing code rather than copying it: Retaskable checks both before it pushes your branch, and sends the work back if not."
        );
        assert_eq!(
            gate_rule(&CodeGates {
                max_file_lines: 0,
                dup_lines: 8,
                max_spec_chars: 0
            }),
            " Reuse the project's existing code rather than copying it: Retaskable checks this before it pushes your branch, and sends the work back if not."
        );
        assert_eq!(
            gate_rule(&CodeGates {
                max_file_lines: 0,
                dup_lines: 0,
                max_spec_chars: 0
            }),
            ""
        );
        assert_eq!(
            gate_rule(&CodeGates {
                max_file_lines: 1000,
                dup_lines: 8,
                max_spec_chars: 40000
            }),
            " Keep every source file you touch at 1000 lines or fewer (split one that's already over), and reuse the project's existing code rather than copying it, and keep every spec you touch under 40000 characters (split one that's over into a section folder with an index): Retaskable checks these before it pushes your branch, and sends the work back if not."
        );
    }

    #[test]
    fn build_prompt_fills_the_implementer_template() {
        let template = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("implementer.md"),
        )
        .unwrap();
        let it = item(&["docs/specs/agent/06.2-implementer.md"], Some("Wire the session."));
        let prompt = build_prompt(
            &template,
            &Prompt {
                item: &it,
                project_type: ProjectType::Cli,
                branch: "retaskable/implementer-session-s2",
                base_branch: "main",
                github_repo: "Retaskable/retaskable-agent",
                checks: &checks(),
                code_gates: CodeGates::default(),
                where_to_start: "## Where to start\n\n- `src/implementer/mod.rs`",
                chore_kind: "",
                chore_instructions: "",
            },
        )
        .unwrap();
        assert!(!prompt.contains("{{"), "no placeholder left unfilled:\n{prompt}");
        assert!(prompt.contains("Item implementer-session: Have a session write the code"));
        assert!(prompt.contains("Wire the session."));
        assert!(prompt.contains("docs/specs/agent/06.2-implementer.md"));
        assert!(prompt.contains("retaskable/implementer-session-s2"));
        assert!(prompt.contains("Retaskable/retaskable-agent"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("status: \"waits_on\"` with `waitsOn`"));
        assert!(prompt.ends_with("- `src/implementer/mod.rs`"));
    }

    #[test]
    fn build_prompt_defaults_remaining_and_spec_refs_when_empty() {
        let template = "{{remaining}} / {{specRefs}}";
        let it = item(&[], None);
        let prompt = build_prompt(
            template,
            &Prompt {
                item: &it,
                project_type: ProjectType::Cli,
                branch: "b",
                base_branch: "main",
                github_repo: "x/y",
                checks: &[],
                code_gates: CodeGates::default(),
                where_to_start: "",
                chore_kind: "",
                chore_instructions: "",
            },
        )
        .unwrap();
        assert_eq!(prompt, "(nothing listed) / (none)");
    }

    #[test]
    fn build_prompt_fills_the_chore_template() {
        let template = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("chore.md"),
        )
        .unwrap();
        let it = item(&[], None);
        let prompt = build_prompt(
            &template,
            &Prompt {
                item: &it,
                project_type: ProjectType::Cli,
                branch: "retaskable/chore-1-tidy-up-s1",
                base_branch: "main",
                github_repo: "Retaskable/retaskable-agent",
                checks: &checks(),
                code_gates: CodeGates::default(),
                where_to_start: "",
                chore_kind: "tidy",
                chore_instructions: "Find one coherent area with warnings, dead code or duplication and clean it up.",
            },
        )
        .unwrap();
        assert!(!prompt.contains("{{"), "no placeholder left unfilled:\n{prompt}");
        assert!(prompt.contains("tidy"));
        assert!(prompt.contains("Find one coherent area"));
        assert!(prompt.contains("retaskable/chore-1-tidy-up-s1"));
        assert!(prompt.contains("cargo test"));
    }
}
