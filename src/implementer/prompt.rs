//! Fills `prompts/implementer.md` (agent/src/prompts.ts's `backlogVars`/`projectVars`/`hostVars`,
//! and `checksText`/`gateRule` in `agent/src/steps.ts`/`codeGates.ts`, combined into one builder for
//! this agent's own [`super::model::ImplementerResult`]/[`super::model::implementer_result_schema`]).
//! This agent has no code-host abstraction yet ([`agent.md`] section 3.4 names only GitHub's `gh`),
//! so `{{pr}}`/`{{change}}` are hardcoded to GitHub's words rather than built from a config.
use anyhow::Result;

use crate::backlog::runs::{RunKind, RunPiece};
use crate::backlog::{Item, ItemStatus};
use crate::config::{CheckConfig, CodeGates, ProjectType};
use crate::decisions::{Decision, decisions_text};
use crate::prompt::{project_words, render_prompt};
use crate::text::capitalize;

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
    /// A Start over's notes: put under "Notes from the last attempt" in the first prompt.
    pub notes: Option<&'a str>,
    /// The project's items as the store listed them: [`not_built_yet`] names the unbuilt others.
    pub items: &'a [Item],
}

/// The ids of `items` other than `this` that aren't built yet: the only ones `waitsOn` may name.
pub fn not_built_yet<'a>(items: &'a [Item], this: &str) -> impl Iterator<Item = &'a Item> {
    items.iter().filter(move |i| {
        i.id != this
            && !matches!(
                i.status,
                ItemStatus::OnMain | ItemStatus::InProduction | ItemStatus::Skipped
            )
    })
}

/// The `## Items not built yet` section (`- <id>: <title>` lines), or empty when none are left.
pub fn items_not_built_text(items: &[Item], this: &str) -> String {
    let lines: Vec<String> = not_built_yet(items, this)
        .map(|i| format!("- {}: {}", i.id, i.title))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "## Items not built yet\n\nThe backlog's other items not built yet (id: title), for `waitsOn`:\n{}",
        lines.join("\n")
    )
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
        ],
    )?;
    let mut out = filled;
    let notes = p
        .notes
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| format!("## Notes from the last attempt\n\nA person started this slice over. Their notes:\n\n{n}"))
        .unwrap_or_default();
    for extra in [
        notes,
        p.where_to_start.to_string(),
        items_not_built_text(p.items, &p.item.id),
    ] {
        if !extra.is_empty() {
            out = format!("{out}\n\n{extra}");
        }
    }
    Ok(out)
}

/// A piece's attempts as the store keeps them, one line each: `- the <kind> <outcome>: <why>`.
fn attempts_text(attempts: &[serde_json::Value]) -> String {
    let word = |a: &serde_json::Value, k: &str| a.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let lines: Vec<String> = attempts
        .iter()
        .map(|a| format!("- the {} {}: {}", word(a, "kind"), word(a, "outcome"), word(a, "why")))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "## Earlier attempts\n\nThese runs on this piece did not finish it. Don't repeat what they got wrong:\n\n{}",
        lines.join("\n")
    )
}

/// The person's answers on a piece (`decisions`: `{question, answer}` as the store sends them in the piece's other
/// fields), as the "Already decided by a person" section.
fn piece_decisions(piece: &RunPiece) -> String {
    decisions_text(&piece_decision_list(piece)).trim_start().to_string()
}

/// The person's answers on a piece, as [`Decision`]s.
pub fn piece_decision_list(piece: &RunPiece) -> Vec<Decision> {
    let text = |d: &serde_json::Value, k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    piece
        .other
        .get("decisions")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .map(|d| Decision {
            question: text(d, "question"),
            answer: text(d, "answer"),
            at: String::new(),
        })
        .collect()
}

/// "Already done: <steps>" for the person steps a person has finished (`doneSteps` in the piece's other fields).
fn done_steps_text(piece: &RunPiece) -> String {
    let steps: Vec<&str> = piece
        .other
        .get("doneSteps")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .collect();
    if steps.is_empty() {
        return String::new();
    }
    format!(
        "Already done: {}. A person has done these; don't ask for them again.",
        steps.join("; ")
    )
}

/// What the latest look that gave `instructions` told the runs (`looks[]` in the piece's other fields).
fn look_text(piece: &RunPiece) -> String {
    piece
        .other
        .get("looks")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .rev()
        .filter_map(|l| l.get("instructions").and_then(|i| i.as_str()))
        .map(str::trim)
        .find(|i| !i.is_empty())
        .map(|i| format!("## What a look said\n\nA look at this piece's earlier runs told the next runs:\n\n{i}"))
        .unwrap_or_default()
}

/// What a piece's `build` or `fix` session is built from: the piece is the brief.
pub struct PiecePrompt<'a> {
    pub kind: RunKind,
    pub piece: &'a RunPiece,
    pub project_type: ProjectType,
    pub branch: &'a str,
    pub base_branch: &'a str,
    pub github_repo: &'a str,
    pub checks: &'a [CheckConfig],
    pub code_gates: CodeGates,
    /// [`super::start_files::render`]'s output; empty for none.
    pub where_to_start: &'a str,
}

/// Fills `prompts/implementer-piece.md` for a fresh `build` or `fix` session. `next.what` is what to fix (for a
/// `fix`); a look's `instructions` come from `looks[]`.
pub fn build_piece_prompt(template: &str, p: &PiecePrompt<'_>) -> Result<String> {
    let words = project_words(p.project_type);
    let piece = p.piece;
    let next_what = piece
        .other
        .get("next")
        .and_then(|n| n.get("what"))
        .and_then(|w| w.as_str())
        .map(str::trim)
        .filter(|w| !w.is_empty());
    let fixing = p.kind == RunKind::Fix;
    let kind_line = if fixing {
        "`fix`. The work is on the piece's pushed branch below, with its change already open: check it out, don't start again from the base."
    } else {
        "`build`. A fresh branch from the base."
    };
    let brief = match (next_what, fixing) {
        (Some(w), true) => format!("## What to fix\n\n{w}"),
        (None, true) => {
            "## What to fix\n\nThe change needs fixing; read its failing checks and conflicts on the pushed branch."
                .to_string()
        }
        (Some(w), false) => format!("## Instructions\n\n{w}"),
        (None, false) => String::new(),
    };
    let mut sections = vec![
        brief,
        look_text(piece),
        attempts_text(&piece.attempts),
        piece_decisions(piece),
        done_steps_text(piece),
    ];
    sections.retain(|s| !s.is_empty());
    let briefs = if sections.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", sections.join("\n\n"))
    };
    let filled = render_prompt(
        template,
        &[
            ("pr", "PR".to_string()),
            ("change", "pull request".to_string()),
            ("title", piece.title.clone()),
            ("what", piece.what.clone()),
            ("doneWhen", piece.done_when.clone()),
            ("kindLine", kind_line.to_string()),
            ("briefSection", briefs),
            ("branch", p.branch.to_string()),
            ("baseBranch", p.base_branch.to_string()),
            ("githubRepo", p.github_repo.to_string()),
            ("checks", checks_text(p.checks)),
            ("codeLimits", gate_rule(&p.code_gates)),
            ("projectNoun", words.noun.to_string()),
            ("projectUser", words.user.to_string()),
            ("projectExamples", words.examples.to_string()),
            ("projectSteps", words.steps.to_string()),
        ],
    )?;
    Ok(if p.where_to_start.is_empty() {
        filled
    } else {
        format!("{filled}\n\n{}", p.where_to_start)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::implementer::test_support::{checks, item};

    fn other(id: &str, status: ItemStatus) -> Item {
        Item {
            id: id.into(),
            title: format!("Title of {id}"),
            status,
            ..item(&[], None)
        }
    }

    #[test]
    fn items_not_built_text_leaves_out_this_item_and_built_ones() {
        let this = item(&[], None);
        let items = vec![
            this.clone(),
            other("a", ItemStatus::Todo),
            other("b", ItemStatus::OnMain),
            other("c", ItemStatus::InProduction),
            other("d", ItemStatus::Building),
        ];
        let text = items_not_built_text(&items, &this.id);
        assert!(text.starts_with("## Items not built yet"));
        assert!(text.ends_with("- a: Title of a\n- d: Title of d"), "{text}");
        assert!(!text.contains(&format!("- {}:", this.id)) && !text.contains("- b:") && !text.contains("- c:"));
        assert_eq!(items_not_built_text(&items[..1], &this.id), "");
        assert!(!items_not_built_text(&items[..1], "zzz").is_empty());
    }

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
                notes: None,
                items: &[],
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
                notes: None,
                items: &[],
            },
        )
        .unwrap();
        assert_eq!(prompt, "(nothing listed) / (none)");
    }

    fn piece(extra: serde_json::Value) -> RunPiece {
        let mut v = serde_json::json!({
            "title": "Build the prompt",
            "what": "Fill the template from a piece.",
            "doneWhen": "A spec can be built.",
            "attempts": [{"kind": "build", "outcome": "failed", "why": "too big"}],
        });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    fn piece_prompt(kind: RunKind, p: &RunPiece) -> String {
        let template = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("implementer-piece.md"),
        )
        .unwrap();
        build_piece_prompt(
            &template,
            &PiecePrompt {
                kind,
                piece: p,
                project_type: ProjectType::Cli,
                branch: "retaskable/p-build-the-prompt",
                base_branch: "main",
                github_repo: "x/y",
                checks: &checks(),
                code_gates: CodeGates::default(),
                where_to_start: "## Where to start\n\n- a",
            },
        )
        .unwrap()
    }

    #[test]
    fn a_build_prompt_carries_the_piece_and_what_a_person_and_a_look_added() {
        let p = piece(serde_json::json!({
            "looks": [
                {"decision": "start_over", "reason": "r", "instructions": "Old."},
                {"decision": "again", "reason": "r", "instructions": "Use the small template."},
            ],
            "decisions": [{"question": "Which?", "answer": "This one."}],
            "doneSteps": ["Add the secret", "Create the account"],
        }));
        let out = piece_prompt(RunKind::Build, &p);
        assert!(!out.contains("{{"), "{out}");
        for want in [
            "Build the prompt",
            "Fill the template from a piece.",
            "Done when: A spec can be built.",
            "- the build failed: too big",
            "## Already decided by a person",
            "A: This one.",
            "Already done: Add the secret; Create the account",
            "## What a look said",
            "Use the small template.",
            "Build this piece whole, and nothing else",
            "return `failed` and say in `summary` how it should be split",
            "retaskable/p-build-the-prompt",
            "- This run: `build`. A fresh branch from the base.",
            "open no PR: return `status: \"nothing_left\"`",
            "rustfmt --edition <the crate's edition> <file>",
        ] {
            assert!(out.contains(want), "missing {want:?}:\n{out}");
        }
        assert!(!out.contains("pushed branch") && !out.contains("Old."));
        assert!(out.ends_with("- a"));
    }

    #[test]
    fn a_fix_prompt_says_what_to_fix_on_the_pushed_branch() {
        let p = piece(serde_json::json!({
            "next": {"kind": "fix", "what": "CI failed: clippy."},
            "looks": [{"decision": "again", "reason": "r", "instructions": "Keep the fix small."}],
        }));
        let out = piece_prompt(RunKind::Fix, &p);
        assert!(!out.contains("{{"));
        assert!(out.contains(
            "## What a look said\n\nA look at this piece's earlier runs told the next runs:\n\nKeep the fix small."
        ));
        assert!(out.contains("## What to fix\n\nCI failed: clippy."));
        assert!(out.contains("pushed branch"));
        let bare = piece_prompt(RunKind::Fix, &piece(serde_json::json!({"attempts": []})));
        assert!(bare.contains("## What to fix") && !bare.contains("{{"));
    }
}
