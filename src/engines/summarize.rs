//! Turns one stream-json line into short, readable events for a UI drawer, following
//! `agent/src/summarize.ts` in Retaskable.com and docs/specs/engines/02-the-engine-layer.md. This
//! is Claude Code's own line shape; Cursor's differs (docs/specs/engines/08-live-transcripts.md)
//! and gets its own `summarize` once that engine is built.
use serde_json::Value;

use super::engine::{EventKind, SessionEvent};
use crate::text::{clip, collapse_and_clip};

const TOOL_ARG_KEYS: [&str; 8] = [
    "command",
    "file_path",
    "notebook_path",
    "pattern",
    "url",
    "query",
    "description",
    "prompt",
];

/// `toolText(name, input)`: the tool name plus its first argument that matches (in
/// `TOOL_ARG_KEYS` order), collapsed to single spaces and clipped to 160 characters.
/// `StructuredOutput`'s own input is never shown.
fn tool_text(name: &str, input: Option<&Value>) -> String {
    if name == "StructuredOutput" {
        return "Result: structured output".to_string();
    }
    let arg = TOOL_ARG_KEYS
        .iter()
        .find_map(|key| input.and_then(|i| i.get(key)).and_then(Value::as_str))
        .unwrap_or("");
    format!("{name}: {}", collapse_and_clip(arg, 160))
}

/// `summarize(ev, at)`: an `assistant` event's tool calls and text become `tool`/`text` events; a
/// `result` event becomes one `Finished: …` event; anything else gives none.
pub fn summarize(event: &Value, at: i64) -> Vec<SessionEvent> {
    match event.get("type").and_then(Value::as_str) {
        Some("assistant") => event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|c| match c.get("type").and_then(Value::as_str) {
                        Some("tool_use") => Some(SessionEvent {
                            at,
                            kind: EventKind::Tool,
                            text: tool_text(c.get("name").and_then(Value::as_str).unwrap_or(""), c.get("input")),
                        }),
                        Some("text") => {
                            let text = c.get("text").and_then(Value::as_str).unwrap_or("").trim();
                            if text.is_empty() {
                                None
                            } else {
                                Some(SessionEvent {
                                    at,
                                    kind: EventKind::Text,
                                    text: clip(text, 800),
                                })
                            }
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Some("result") => {
            let subtype = event.get("subtype").and_then(Value::as_str).unwrap_or("");
            let text = if subtype == "success" { "done" } else { subtype };
            vec![SessionEvent {
                at,
                kind: EventKind::Result,
                text: format!("Finished: {text}"),
            }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_use_becomes_the_tool_name_and_its_first_matching_argument() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": "ls -la"}}]},
        });
        assert_eq!(
            summarize(&ev, 1),
            vec![SessionEvent {
                at: 1,
                kind: EventKind::Tool,
                text: "Bash: ls -la".to_string()
            }]
        );
    }

    #[test]
    fn tool_use_tries_each_argument_key_in_order() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Grep", "input": {"pattern": "foo", "url": "ignored"}}]},
        });
        assert_eq!(summarize(&ev, 1)[0].text, "Grep: foo");
    }

    #[test]
    fn tool_use_with_no_matching_argument_or_no_input_shows_an_empty_argument() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Mystery", "input": {"other": "x"}}]},
        });
        assert_eq!(summarize(&ev, 1)[0].text, "Mystery: ");

        let no_input = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Mystery"}]},
        });
        assert_eq!(summarize(&no_input, 1)[0].text, "Mystery: ");
    }

    #[test]
    fn structured_output_never_shows_its_input() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "StructuredOutput", "input": {"status": "pr_opened"}}]},
        });
        assert_eq!(summarize(&ev, 1)[0].text, "Result: structured output");
    }

    #[test]
    fn tool_use_argument_whitespace_collapses_to_single_spaces_and_clips_at_160() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": "a\n\n  b   c"}}]},
        });
        assert_eq!(summarize(&ev, 1)[0].text, "Bash: a b c");

        let long = "x".repeat(200);
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": long}}]},
        });
        let expected = format!("Bash: {}…", "x".repeat(159));
        assert_eq!(summarize(&ev, 1)[0].text, expected);
    }

    #[test]
    fn text_blocks_trim_and_clip_at_800_blank_ones_are_skipped() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "  hello  "}, {"type": "text", "text": "   "}]},
        });
        assert_eq!(
            summarize(&ev, 5),
            vec![SessionEvent {
                at: 5,
                kind: EventKind::Text,
                text: "hello".to_string()
            }]
        );
    }

    #[test]
    fn multiple_content_blocks_give_one_event_each_in_order() {
        let ev = json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "text", "text": "thinking"},
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
            ]},
        });
        let events = summarize(&ev, 1);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, EventKind::Text);
        assert_eq!(events[1].kind, EventKind::Tool);
    }

    #[test]
    fn no_content_or_message_gives_no_events() {
        assert_eq!(summarize(&json!({"type": "assistant"}), 1), vec![]);
        assert_eq!(summarize(&json!({"type": "assistant", "message": {}}), 1), vec![]);
    }

    #[test]
    fn result_events_say_done_on_success_or_the_subtype_otherwise() {
        let ev = json!({"type": "result", "subtype": "success"});
        assert_eq!(
            summarize(&ev, 9),
            vec![SessionEvent {
                at: 9,
                kind: EventKind::Result,
                text: "Finished: done".to_string()
            }]
        );

        let ev = json!({"type": "result", "subtype": "error_during_execution"});
        assert_eq!(summarize(&ev, 9)[0].text, "Finished: error_during_execution");
    }

    #[test]
    fn any_other_event_type_gives_no_events() {
        assert_eq!(summarize(&json!({"type": "rate_limit_event"}), 1), vec![]);
        assert_eq!(summarize(&json!({}), 1), vec![]);
    }
}
