//! Fills `prompts/implementer.md` (agent/src/prompts.ts's `backlogVars`/`projectVars`/`hostVars`,
//! and `checksText`/`gateRule` in `agent/src/steps.ts`/`codeGates.ts`, combined into one builder for
//! this agent's own [`super::model::ImplementerResult`]/[`super::model::implementer_result_schema`]).
//! This agent has no code-host abstraction yet ([`agent.md`] section 3.4 names only GitHub's `gh`),
//! so `{{pr}}`/`{{change}}` are hardcoded to GitHub's words rather than built from a config.
use anyhow::Result;

use serde_json::Value;

use crate::backlog::runs::{RunKind, RunPiece};
use crate::backlog::{Item, ItemStatus};
use crate::config::{CheckConfig, CodeGates, ProjectType};
use crate::decisions::{Decision, decisions_text};
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

const WHOLE_SLICE: &str = "   **The whole slice, in as little code as it needs.** Be thorough about scope and sparing about code: reuse the project's own helpers and patterns before writing new ones, and the language's or an existing dependency's before adding one; write no abstraction, option, config or \"for later\" code the spec doesn't ask for; fix a bug once, at its root, after finding every caller. Never cut validation, error handling, security, accessibility or anything the spec asks for.";
const ITEM_RULE_1: &str = "**Pick one slice, and make it a whole one.** When the spec divides the work into slices or steps, your slice is the next of those, all of it, in this one PR: don't split a planned slice across several. Otherwise choose the next coherent piece of the remaining work, and if the whole item fits in one PR, do it all. A PR of 500 to 2,000 changed lines is a good size; going smaller only adds rounds, since every PR waits for its checks, a review and a merge whatever its size (2026-10-21: one item went out as 41 PRs of 150 to 400 lines, against the 5 slices its spec planned). Split further only when a planned slice really can't be finished, or the project left working and testable, in one go, and then say why in `sliceDescription`. Prefer a slice that leaves the project working and testable.";

/// The template's `{{header}}`: the item's lines, with the branch and repo.
fn item_header(p: &Prompt<'_>) -> String {
    let remaining = p
        .item
        .remaining
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("(nothing listed)");
    let refs = if p.item.spec_refs.is_empty() {
        "(none)".to_string()
    } else {
        p.item.spec_refs.join(", ")
    };
    format!(
        "- **Item {}: {}**\n- Slice number: {}\n- {REMAINING_LABEL}: {remaining}\n- Specs it links to: {refs}\n{}",
        p.item.id,
        p.item.title,
        p.item.slice,
        branch_lines(p.branch, p.base_branch, p.github_repo)
    )
}

fn branch_lines(branch: &str, base: &str, repo: &str) -> String {
    format!(
        "- Your branch: `{branch}`, based on `origin/{base}`, in this directory (a git worktree of its own).\n- GitHub repo: `{repo}`"
    )
}

/// The variables both forms share; each form adds its own `header`, `rule1` and the item words.
fn shared_vars(
    project_type: ProjectType,
    branch: &str,
    base_branch: &str,
    github_repo: &str,
    checks: &[CheckConfig],
    gates: &CodeGates,
) -> Vec<(&'static str, String)> {
    let words = project_words(project_type);
    vec![
        ("pr", "PR".to_string()),
        ("change", "pull request".to_string()),
        ("sourceOfTruth", SOURCE_OF_TRUTH.to_string()),
        ("branch", branch.to_string()),
        ("baseBranch", base_branch.to_string()),
        ("githubRepo", github_repo.to_string()),
        ("checks", checks_text(checks)),
        ("codeLimits", gate_rule(gates)),
        ("projectNoun", words.noun.to_string()),
        ("projectUser", words.user.to_string()),
        ("projectExamples", words.examples.to_string()),
        ("projectSteps", words.steps.to_string()),
    ]
}

/// Appends each non-empty `extra` section after the rules.
fn append_sections(mut out: String, extras: &[String]) -> String {
    for extra in extras.iter().filter(|e| !e.is_empty()) {
        out = format!("{out}\n\n{extra}");
    }
    out
}

/// Fills `prompts/implementer.md` for a fresh session on `p.item`.
pub fn build_prompt(template: &str, p: &Prompt<'_>) -> Result<String> {
    let mut vars = shared_vars(
        p.project_type,
        p.branch,
        p.base_branch,
        p.github_repo,
        p.checks,
        &p.code_gates,
    );
    vars.extend([
        ("header", item_header(p)),
        ("rule1", format!("{ITEM_RULE_1}\n{WHOLE_SLICE}")),
        ("itemId", p.item.id.clone()),
        ("title", p.item.title.clone()),
        ("slice", p.item.slice.to_string()),
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
        ("remainingLabel", REMAINING_LABEL.to_string()),
        ("backlogLine", String::new()),
        ("itemEntry", ITEM_ENTRY.to_string()),
        ("entryShort", ENTRY_SHORT.to_string()),
        ("nothingLeft", NOTHING_LEFT.to_string()),
        ("nothingToChange", NOTHING_TO_CHANGE.to_string()),
        ("backlogRule", backlog_rule()),
        ("doneReturn", DONE_RETURN.to_string()),
    ]);
    let filled = render_prompt(template, &vars)?;
    let notes = p
        .notes
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| format!("## Notes from the last attempt\n\nA person started this slice over. Their notes:\n\n{n}"))
        .unwrap_or_default();
    Ok(append_sections(
        filled,
        &[
            notes,
            p.where_to_start.to_string(),
            items_not_built_text(p.items, &p.item.id),
        ],
    ))
}

const PIECE_RULE_1: &str = "**Build your piece, all of it, and nothing else.** The item was planned into pieces that other sessions build, often at the same time; yours is the one above, and its \"Done when\" is how your reviewer will judge it. Don't start on the item's other pieces, even ones that look small. The store, not your result, says what is left of the item. If the piece really can't be finished in one PR with the project left working and testable, return `failed` and say in `summary` how it should be split.";
const PIECE_ENTRY: &str = "the piece above (its title, what it is and when it's done)";
const PIECE_NOTHING_LEFT: &str = "and you return `itemDone: false`";
const PIECE_NOTHING_TO_CHANGE: &str = " If there isn't even a spec to update (the specs on the base branch already have your piece built, or name the later piece or item that now has the rest of it), open no PR and commit nothing new: return `status: \"nothing_left\"`, with `summary` saying where it's built. Work already committed on your branch by an earlier session isn't on the base branch: it's your change, so return `pr_opened`. Setup only a person can do isn't nothing left: that's `failed`, saying what they have to do. Don't ask a question whose answer the specs already give. When all of the piece's code is on the base branch and only steps a person can do are left (an account, a secret, a setting in production, an invitation), open no PR: return `status: \"person_steps\"` with `personSteps`, each step one short sentence a person can follow, in order. If a person's note says they've done the steps, believe them.";
const PIECE_BACKLOG_RULE: &str = "**Don't edit a backlog file.** Update any spec the piece changes to describe what is now built, in its own \"as built\" style, but there's no backlog file to change: Retaskable keeps the backlog and the item's other pieces. Return `itemDone: false` and leave `remaining` empty.";

/// What a fresh implementer prompt for a piece's `build` or `fix` run is built from, besides what
/// [`Prompt`] has: the run's kind and the piece as the store sent it ([`RunPiece`]). The item's
/// `id`/`title` come from `item`, whose `slice` and `remaining` the piece form ignores.
pub struct PiecePrompt<'a> {
    pub prompt: Prompt<'a>,
    pub kind: RunKind,
    pub piece: &'a RunPiece,
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The person's answers on the piece (`answers`: `{ question, answer }` each), as decisions.
fn piece_decisions(piece: &RunPiece) -> Vec<Decision> {
    piece
        .other
        .get("answers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|a| !str_of(a, "question").is_empty() && !str_of(a, "answer").is_empty())
        .map(|a| Decision {
            question: str_of(a, "question").to_string(),
            answer: str_of(a, "answer").to_string(),
            at: String::new(),
        })
        .collect()
}

/// `Already done: <steps>` for the person steps a person has finished (`doneSteps`), or empty.
fn done_steps_text(piece: &RunPiece) -> String {
    let steps: Vec<&str> = piece
        .other
        .get("doneSteps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if steps.is_empty() {
        String::new()
    } else {
        format!("Already done: {}", steps.join("; "))
    }
}

/// The `## Earlier attempts` section: each attempt's kind, outcome and why.
fn attempts_text(piece: &RunPiece) -> String {
    let lines: Vec<String> = piece
        .attempts
        .iter()
        .map(|a| {
            format!(
                "- the {} {}: {}",
                str_of(a, "kind"),
                str_of(a, "outcome"),
                str_of(a, "why")
            )
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "## Earlier attempts at this piece\n\nDon't repeat what went wrong:\n{}",
        lines.join("\n")
    )
}

/// What the latest look that gave `instructions` told the runs, or empty.
fn look_text(piece: &RunPiece) -> String {
    piece
        .other
        .get("looks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .map(|l| str_of(l, "instructions"))
        .find(|i| !i.is_empty())
        .map(|i| format!("## Instructions from a look at this piece\n\n{i}"))
        .unwrap_or_default()
}

/// The piece's `## The piece` header; a `fix` run also says what to fix and where the work is.
fn piece_header(pp: &PiecePrompt<'_>) -> String {
    let (p, piece) = (&pp.prompt, pp.piece);
    let next = piece.other.get("next").map(|n| str_of(n, "what")).unwrap_or("");
    let mut header = format!(
        "**Piece of item {}: {}**\n\n{}\n\nDone when: {}",
        p.item.id, piece.title, piece.what, piece.done_when
    );
    if pp.kind == RunKind::Fix {
        let branch = piece.branch.as_deref().unwrap_or(p.branch);
        header.push_str(&format!(
            "\n\n**This is a fix run.** The work is already on the piece's pushed branch `{branch}`: check it out and carry on from it, don't start over. Fix this:\n\n{}",
            if next.is_empty() { "(the reason wasn't recorded: read the change's checks and review)" } else { next }
        ));
    } else if !next.is_empty() {
        header.push_str(&format!("\n\n{next}"));
    }
    format!("{header}\n{}", branch_lines(p.branch, p.base_branch, p.github_repo))
}

/// Fills the same `prompts/implementer.md` for a fresh `build` or `fix` run on a piece: the piece is the
/// brief (pieces.md section 3.2).
pub fn build_piece_prompt(template: &str, pp: &PiecePrompt<'_>) -> Result<String> {
    let p = &pp.prompt;
    let mut vars = shared_vars(
        p.project_type,
        p.branch,
        p.base_branch,
        p.github_repo,
        p.checks,
        &p.code_gates,
    );
    vars.extend([
        ("header", piece_header(pp)),
        ("rule1", format!("{PIECE_RULE_1}\n{WHOLE_SLICE}")),
        ("itemId", p.item.id.clone()),
        ("title", pp.piece.title.clone()),
        ("slice", String::new()),
        ("remaining", String::new()),
        ("specRefs", p.item.spec_refs.join(", ")),
        ("remainingLabel", REMAINING_LABEL.to_string()),
        ("backlogLine", String::new()),
        ("itemEntry", PIECE_ENTRY.to_string()),
        ("entryShort", "the piece".to_string()),
        ("nothingLeft", PIECE_NOTHING_LEFT.to_string()),
        ("nothingToChange", PIECE_NOTHING_TO_CHANGE.to_string()),
        ("backlogRule", PIECE_BACKLOG_RULE.to_string()),
        ("doneReturn", String::new()),
    ]);
    let filled = render_prompt(template, &vars)?;
    let specs = match p.item.spec_refs.as_slice() {
        [] => String::new(),
        refs => format!("Specs it links to: {}", refs.join(", ")),
    };
    Ok(append_sections(
        filled,
        &[
            specs,
            attempts_text(pp.piece),
            look_text(pp.piece),
            decisions_text(&piece_decisions(pp.piece)).trim_start().to_string(),
            done_steps_text(pp.piece),
            p.where_to_start.to_string(),
            items_not_built_text(p.items, &p.item.id),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::runs::{RunKind, RunPiece};
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

    fn piece_prompt(kind: RunKind, piece: &RunPiece) -> String {
        let template = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("implementer.md"),
        )
        .unwrap();
        let it = item(&["docs/specs/pieces.md"], Some("ignored"));
        let prompt = build_piece_prompt(
            &template,
            &PiecePrompt {
                prompt: Prompt {
                    item: &it,
                    project_type: ProjectType::Cli,
                    branch: "retaskable/implementer-session-p1-t",
                    base_branch: "main",
                    github_repo: "Retaskable/retaskable-agent",
                    checks: &checks(),
                    code_gates: CodeGates::default(),
                    where_to_start: "## Where to start\n\n- `src/x.rs`",
                    notes: None,
                    items: &[],
                },
                kind,
                piece,
            },
        )
        .unwrap();
        assert!(!prompt.contains("{{"), "no placeholder left unfilled:\n{prompt}");
        prompt
    }

    fn piece(extra: serde_json::Value) -> RunPiece {
        let mut v = serde_json::json!({
            "title": "Build the prompt", "what": "Fill it from a piece.", "doneWhen": "A spec builds.",
            "attempts": [{ "kind": "build", "outcome": "failed", "why": "Too big for one change" }]
        });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_build_piece_prompt_carries_the_brief_and_what_came_before() {
        let p = piece(serde_json::json!({
            "answers": [{ "question": "Which crate?", "answer": "regex" }],
            "doneSteps": ["Create the account", "Add the secret"],
            "looks": [{ "decision": "again", "reason": "r", "instructions": "Split the helper out first." }]
        }));
        let prompt = piece_prompt(RunKind::Build, &p);
        for want in [
            "Build the prompt",
            "Fill it from a piece.",
            "Done when: A spec builds.",
            "the build failed: Too big for one change",
            "## Already decided by a person",
            "A: regex",
            "Already done: Create the account; Add the secret",
            "Split the helper out first.",
            "Build your piece, all of it, and nothing else",
            "The store, not your result, says what is left",
            "retaskable/implementer-session-p1-t",
        ] {
            assert!(prompt.contains(want), "missing {want:?}:\n{prompt}");
        }
        assert!(!prompt.contains("This is a fix run"));
        assert!(prompt.ends_with("- `src/x.rs`"));
    }

    #[test]
    fn a_fix_piece_prompt_says_what_to_fix_on_the_pushed_branch() {
        let p = piece(serde_json::json!({
            "branch": "retaskable/pushed-p1", "next": { "kind": "fix", "what": "CI failed: cargo test" }
        }));
        let prompt = piece_prompt(RunKind::Fix, &p);
        assert!(prompt.contains("This is a fix run"));
        assert!(prompt.contains("pushed branch `retaskable/pushed-p1`"));
        assert!(prompt.contains("CI failed: cargo test"));
    }

    #[test]
    fn a_bare_piece_prompt_has_no_empty_sections() {
        let p: RunPiece = serde_json::from_value(serde_json::json!({ "title": "T" })).unwrap();
        let prompt = piece_prompt(RunKind::Build, &p);
        for gone in [
            "Earlier attempts",
            "Already decided",
            "Already done",
            "Instructions from a look",
        ] {
            assert!(!prompt.contains(gone), "{gone}");
        }
    }
}
