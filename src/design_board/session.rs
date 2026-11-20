//! The session that answers a layout (TS `designBoard.ts` `designSession`, `designLayoutFor`; schema
//! `DESIGN_BOARD_SCHEMA` in `agent/src/schemas.ts`): a read-only Claude session given the idea, the drawing table
//! and the pages' parts, answering one layout. It reads no file and runs no command: everything it needs is in the
//! prompt, so it runs in a scratch directory with no shell, on the lighter tier (a layout is short).
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use super::catalog::{library_text, pages_text};
use super::place::{BoardRead, board_text};
use crate::engines::engine::{EngineId, LocalTarget, SessionSpec, ephemeral_readonly_spec};
use crate::kit::{Kit, prompt_text};
use crate::prompt::render_prompt;
use crate::text::truncate;

const TEMPLATE: &str = include_str!("../../prompts/design-board.md");

/// A layout as the design board answers it (design-view.md section 5): `check_layout` checks it in plain code against
/// the drawing table.
pub fn layout_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "page": { "type": "string" },
            "region": { "type": "string" },
            "variations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "pieces": {
                            "type": "array",
                            "items": { "type": "object", "properties": {
                                "element": { "type": "string" }, "label": { "type": "string" },
                                "name": { "type": "string" }, "purpose": { "type": "string" } } }
                        }
                    },
                    "required": ["name", "pieces"]
                }
            },
            "replaces": { "type": "array", "items": { "type": "string" } },
            "sentence": { "type": "string" }
        },
        "required": ["variations"]
    })
}

/// The one rewrite of a sentence that failed its check (`DESIGN_SENTENCE_SCHEMA`).
pub fn sentence_schema() -> Value {
    json!({ "type": "object", "properties": { "sentence": { "type": "string" } }, "required": ["sentence"] })
}

const SENTENCE_TEMPLATE: &str = include_str!("../../prompts/design-sentence.md");

/// The prompt that asks for the sentence again: what was wrong with the first (`problems`), what it said (`was`) and the idea.
pub fn sentence_prompt(problems: &[String], was: &str, idea: &Idea<'_>) -> Result<String> {
    let said = idea.summary.filter(|s| !s.is_empty()).unwrap_or(idea.request);
    render_prompt(
        SENTENCE_TEMPLATE,
        &[
            ("problems", problems.join("; ")),
            (
                "was",
                if was.is_empty() {
                    "(none)".into()
                } else {
                    was.to_string()
                },
            ),
            ("idea", format!("{}\n\n{}", idea.title, truncate(said, 2000))),
        ],
    )
}

/// The idea a design is drawn for.
#[derive(Debug, Clone, Default)]
pub struct Idea<'a> {
    pub title: &'a str,
    pub request: &'a str,
    pub summary: Option<&'a str>,
    /// What the person asked to have changed in the designs (**Change it…**).
    pub change: Option<&'a str>,
}

/// The prompt: the library (the project's kit, when it has one of its own), the pages, the board as it is and the idea (with the
/// person's note when they asked for a change).
pub fn layout_prompt(idea: &Idea<'_>, board: &BoardRead, kit: &Kit) -> Result<String> {
    let said = idea.summary.filter(|s| !s.is_empty()).unwrap_or(idea.request);
    let change = match idea.change.filter(|c| !c.is_empty()) {
        Some(c) => format!("\n\nThe person asked for the designs to change: {}", truncate(c, 2000)),
        None => String::new(),
    };
    let (kit_note, library, pages) = prompt_text(kit, library_text, pages_text);
    render_prompt(
        TEMPLATE,
        &[
            ("kitNote", kit_note),
            ("library", library),
            ("pages", pages),
            ("board", board_text(board)),
            ("idea", format!("{}\n\n{}{change}", idea.title, truncate(said, 4000))),
        ],
    )
}

/// The sentence's session: the same read-only, tool-less one as [`layout_spec`], answering [`sentence_schema`].
pub fn sentence_spec(
    prompt: String,
    model: &str,
    budget_usd: f64,
    cwd: PathBuf,
    local: Option<LocalTarget>,
) -> SessionSpec {
    SessionSpec {
        schema: sentence_schema(),
        ..layout_spec(prompt, model, budget_usd, cwd, local)
    }
}

/// The session's `SessionSpec`: Claude on `model` (the scout tier: `haiku`, or `local`), one fresh read-only session in
/// a scratch directory, with no shell and no resume.
pub fn layout_spec(
    prompt: String,
    model: &str,
    budget_usd: f64,
    cwd: PathBuf,
    local: Option<LocalTarget>,
) -> SessionSpec {
    ephemeral_readonly_spec(
        Some(EngineId::Claude),
        model.to_string(),
        prompt,
        layout_schema(),
        cwd,
        budget_usd,
        None,
        local,
        false,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design_board::place::{Piece, Who};
    use crate::engines::engine::SessionStart;
    use crate::kit::library;

    fn note() -> BoardRead {
        BoardRead {
            pieces: vec![Piece {
                id: "note-1".into(),
                kind: "sticky",
                element: None,
                text: Some("Keep it to one tap".into()),
                frame: None,
                who: Who::Person,
                x: 0,
                y: 0,
                w: 200,
                h: 200,
            }],
            selected: vec![],
        }
    }

    #[test]
    fn the_prompt_carries_the_idea_the_pages_the_library_and_the_board() {
        let idea = Idea {
            title: "Snooze an alert",
            request: "Let people snooze",
            summary: Some("A snooze button"),
            change: None,
        };
        let p = layout_prompt(&idea, &note(), &library()).unwrap();
        assert!(
            p.contains("Snooze an alert\n\nA snooze button"),
            "the summary, else the request"
        );
        assert!(p.contains("Keep it to one tap") && p.contains("`header` (header, on desktop and phone)"));
        assert!(p.contains("- `button`: Button (buttons,"));
        assert!(!p.contains("{{") && !p.contains("asked for the designs to change"));
        let blank = Idea {
            summary: Some(""),
            ..idea.clone()
        };
        assert!(
            layout_prompt(&blank, &note(), &library())
                .unwrap()
                .contains("Snooze an alert\n\nLet people snooze")
        );
    }

    #[test]
    fn a_change_asked_for_goes_to_the_session_with_the_idea_and_is_cut() {
        let idea = Idea {
            title: "T",
            request: "r",
            summary: None,
            change: Some("Make it a menu"),
        };
        assert!(
            layout_prompt(&idea, &BoardRead::default(), &library())
                .unwrap()
                .contains("The person asked for the designs to change: Make it a menu")
        );
        let long = "x".repeat(3000);
        let idea = Idea {
            change: Some(&long),
            ..idea
        };
        let p = layout_prompt(&idea, &BoardRead::default(), &library()).unwrap();
        assert!(p.contains("(the board is empty)") && !p.contains(&"x".repeat(2001)));
    }

    #[test]
    fn the_sentence_is_asked_again_with_what_was_wrong_and_what_it_said_and_by_the_same_kind_of_session() {
        let idea = Idea {
            title: "Snooze",
            request: "Let people snooze",
            summary: None,
            change: None,
        };
        let p = sentence_prompt(&["it has a backtick".into(), "it is empty".into()], "", &idea).unwrap();
        assert!(p.contains("refused because: it has a backtick; it is empty"));
        assert!(p.contains("The earlier sentence: (none)") && p.contains("Snooze\n\nLet people snooze"));
        let said = sentence_prompt(&[], "bad `x`", &idea).unwrap();
        assert!(said.contains("The earlier sentence: bad `x`"));
        let spec = sentence_spec("p".into(), "haiku", 2.0, std::env::temp_dir(), None);
        assert!(spec.readonly && !spec.shell);
        assert_eq!(spec.schema["required"], json!(["sentence"]));
    }

    #[test]
    fn the_layouts_schema_asks_for_a_sentence_too() {
        assert_eq!(layout_schema()["properties"]["sentence"], json!({ "type": "string" }));
    }

    #[test]
    fn the_session_is_read_only_fresh_with_no_shell_and_on_the_lighter_model() {
        let spec = layout_spec("p".into(), "haiku", 2.0, std::env::temp_dir(), None);
        assert!(spec.readonly && !spec.shell, "read-only, no command");
        assert_eq!(
            (spec.model.as_str(), spec.engine, spec.budget_usd),
            ("haiku", Some(EngineId::Claude), 2.0)
        );
        assert!(matches!(spec.start, SessionStart::Ephemeral));
        assert_eq!(spec.schema["required"], json!(["variations"]));
    }

    #[test]
    fn a_projects_own_kit_replaces_the_library_in_the_prompt() {
        let idea = Idea {
            title: "T",
            request: "r",
            summary: None,
            change: None,
        };
        let kit = crate::kit::wireframe_kit(crate::kit::Genre::App, None, None, None);
        let p = layout_prompt(&idea, &BoardRead::default(), &kit).unwrap();
        assert!(p.contains("This project is not Retaskable's own") && p.contains("a plain wireframe sketch"));
        assert!(p.contains("- `tabBar`: Tab bar (tabBar)") && !p.contains("- `button`: Button (buttons,"));
        assert!(p.contains("this project's screens are not known") && !p.contains("{{"));
    }
}
