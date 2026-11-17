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
            "replaces": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["variations"]
    })
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

/// The prompt: the library, the pages, the board as it is and the idea (with the person's note when they asked for a
/// change).
pub fn layout_prompt(idea: &Idea<'_>, board: &BoardRead) -> Result<String> {
    let said = idea.summary.filter(|s| !s.is_empty()).unwrap_or(idea.request);
    let change = match idea.change.filter(|c| !c.is_empty()) {
        Some(c) => format!("\n\nThe person asked for the designs to change: {}", truncate(c, 2000)),
        None => String::new(),
    };
    render_prompt(
        TEMPLATE,
        &[
            ("library", library_text()),
            ("pages", pages_text()),
            ("board", board_text(board)),
            ("idea", format!("{}\n\n{}{change}", idea.title, truncate(said, 4000))),
        ],
    )
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
        let p = layout_prompt(&idea, &note()).unwrap();
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
            layout_prompt(&blank, &note())
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
            layout_prompt(&idea, &BoardRead::default())
                .unwrap()
                .contains("The person asked for the designs to change: Make it a menu")
        );
        let long = "x".repeat(3000);
        let idea = Idea {
            change: Some(&long),
            ..idea
        };
        let p = layout_prompt(&idea, &BoardRead::default()).unwrap();
        assert!(p.contains("(the board is empty)") && !p.contains(&"x".repeat(2001)));
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
}
