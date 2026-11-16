//! The handover of a continued Claude Code session (docs/specs/expert-chat.md section 3.10): the session isn't
//! resumed, so its engine files stay its own. The fresh session's first message holds the latest part of the
//! conversation, where the session worked and what git says there, then the person's message.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{describe, lines, person_text, session_file, status_of};
use crate::engines::summarize::tool_text;
use crate::text::{clip, tail};

/// The conversation a handover carries, newest kept (`handoverChars`).
const HANDOVER_CHARS: usize = 40_000;

/// The Claude Code session an expert session continues: its file and where it worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Continued {
    pub session_id: String,
    pub file: PathBuf,
    pub title: String,
    pub folder: String,
    pub branch: Option<String>,
}

/// The session `id` under `root`; says why it can't be continued.
pub fn continuing(root: &Path, id: &str) -> Result<Continued, String> {
    let file = session_file(root, id).ok_or("That Claude Code session isn't on this computer any more.")?;
    let s = describe(&file, &[]).ok_or("That Claude Code session has none of your messages to continue from.")?;
    Ok(Continued {
        session_id: id.to_string(),
        file,
        title: s.title,
        folder: s.folder,
        branch: s.branch,
    })
}

/// The person's messages and Claude's replies in order, each tool call one line and no tool output, newest kept up to
/// `chars`; cut short, it starts "(Earlier conversation left out.)".
fn conversation(file: &Path, chars: usize) -> std::io::Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for ev in lines(&std::fs::read_to_string(file)?) {
        let said = person_text(&ev);
        if !said.is_empty() {
            parts.push(format!("Person: {said}"));
        } else if ev.get("type").and_then(Value::as_str) == Some("assistant")
            && ev.get("isSidechain").and_then(Value::as_bool) != Some(true)
        {
            for c in ev
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match c.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let text = c.get("text").and_then(Value::as_str).unwrap_or("").trim();
                        if !text.is_empty() {
                            parts.push(format!("Claude: {text}"));
                        }
                    }
                    Some("tool_use") => parts.push(format!(
                        "- {}",
                        tool_text(c.get("name").and_then(Value::as_str).unwrap_or(""), c.get("input"))
                    )),
                    _ => {}
                }
            }
        }
    }
    let mut kept: Vec<String> = Vec::new();
    let mut left = chars;
    for part in parts.iter().rev() {
        let n = part.chars().count() + 1;
        if n > left {
            // The newest message alone is longer than the whole: its end is kept.
            if kept.is_empty() {
                kept.push(format!("…{}", tail(part, left.saturating_sub(1))));
            }
            kept.push("(Earlier conversation left out.)".into());
            kept.reverse();
            return Ok(kept.join("\n"));
        }
        kept.push(part.clone());
        left -= n;
    }
    kept.reverse();
    Ok(kept.join("\n"))
}

/// The first message of an expert session continuing the session in `c`, around the person's `request`. `None` when
/// its file can't be read any more: the person's message alone then.
pub fn handover(c: &Continued, request: &str) -> Option<String> {
    handover_of(c, request, HANDOVER_CHARS)
}

fn handover_of(c: &Continued, request: &str, chars: usize) -> Option<String> {
    let talk = conversation(&c.file, chars).ok()?;
    let status = status_of(Path::new(&c.folder));
    let on = c.branch.as_ref().map(|b| format!(" on branch {b}")).unwrap_or_default();
    let git = if status.is_empty() {
        "clean.".to_string()
    } else {
        format!("\n{}", clip(&status, 4000))
    };
    Some(format!(
        "This continues a Claude Code session the person had on this computer. The latest part of its conversation:\n\n\
         {talk}\n\nIt worked in {}{on}. `git status --short` there: {git}\n\nThe person's message now:\n{request}",
        c.folder
    ))
}

#[cfg(test)]
pub(super) fn handover_chars(c: &Continued, request: &str, chars: usize) -> Option<String> {
    handover_of(c, request, chars)
}
