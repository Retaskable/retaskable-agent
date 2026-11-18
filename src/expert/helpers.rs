//! An expert session's helpers (docs/specs/expert-chat.md "Helpers"; Retaskable.com's `agent/src/expert/helpers.ts`):
//! separate sessions Retaskable starts for it on the lighter tier, whose result comes back to the session as a message.
//! A `read` helper runs read-only in the session's own lane; a `lane` helper waits for a free lane and works there on
//! a branch of its own, from the session's last commit. The words and shapes are here; they run in
//! `orchestrator::driver::experts`.
use serde_json::{Value, json};

use crate::prompt::render_prompt;
use crate::text::clip;

/// The most helpers one turn asks for, and the longest task.
pub const ASKED_MAX: usize = 4;
const TASK_MAX: usize = 4000;
/// The longest result a helper's end gives the session, and the history's line.
const RESULT_MAX: usize = 8000;
const NOTE_MAX: usize = 600;

const READ: &str = include_str!("../../prompts/helper.md");
const LANE: &str = include_str!("../../prompts/helper-lane.md");

/// What a helper's result looks like: `{ result }`.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "result": { "type": "string", "description": "What you found, for the session that asked: plain and short, with paths and lines." } },
        "required": ["result"],
        "additionalProperties": false
    })
}

/// A helper a turn asked for: its task and whether it has a lane of its own (`lane: true`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub task: String,
    pub lane: bool,
}

/// The helpers a turn's structured result asks for (`helpersAsked`): at most four, each task cut to size.
pub fn asked(structured: &Value) -> Vec<Asked> {
    (structured
        .get("helpers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten())
    .filter_map(|h| {
        let task = h.get("task")?.as_str()?.trim();
        (!task.is_empty()).then(|| Asked {
            task: clip(task, TASK_MAX),
            lane: h.get("lane").and_then(Value::as_bool) == Some(true),
        })
    })
    .take(ASKED_MAX)
    .collect()
}

/// Helper `n`'s name, as its session's history calls it.
pub fn name(n: u32) -> String {
    format!("helper-{n}")
}

/// A helper's branch: `<the session's branch>-h<n>`.
pub fn branch(from: &str, n: u32) -> String {
    format!("{from}-h{n}")
}

/// How helper `n`'s end lines name it, which the app reads.
pub fn what(n: u32, task: &str) -> String {
    format!("Helper {n} (\"{}\")", clip(task, 80))
}

/// The history line when helper `n` starts.
pub fn started(n: u32, task: &str) -> String {
    format!("Helper {n} started: {}", clip(task, 200))
}

/// What the helper is told.
pub fn prompt(task: &str, branch: &str, own: Option<&str>) -> anyhow::Result<String> {
    match own {
        Some(from) => render_prompt(
            LANE,
            &[
                ("branch", branch.to_string()),
                ("from", from.to_string()),
                ("task", task.to_string()),
            ],
        ),
        None => render_prompt(READ, &[("branch", branch.to_string()), ("task", task.to_string())]),
    }
}

/// How a helper ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    Finished(String),
    /// Didn't finish, and why.
    Failed(String),
    Stopped,
}

/// What a helper's end says: `(for the history, for the session)`, or `None` for a stop that tells the session nothing.
/// A lane helper's commits are on `branch`, which the tail says (`finish`).
pub fn end_lines(n: u32, task: &str, ended: &Ended, own_branch: Option<&str>) -> (String, Option<String>) {
    let what = what(n, task);
    match ended {
        Ended::Stopped => {
            let kept = own_branch.map_or(String::new(), |b| format!(": anything it committed is on {b}"));
            (format!("{what} was stopped{kept}"), None)
        }
        Ended::Finished(result) => finish(
            &what,
            &format!("finished: {}", clip(result.trim(), RESULT_MAX)),
            own_branch,
            true,
        ),
        Ended::Failed(why) => finish(&what, &format!("didn't finish: {}", clip(why, 300)), own_branch, false),
    }
}

fn finish(what: &str, said: &str, own_branch: Option<&str>, done: bool) -> (String, Option<String>) {
    let text = format!("{what} {said}");
    let tail = own_branch.map_or(String::new(), |b| {
        format!(
            "\n\n{} on {b}, from your last commit: merge it in if you want them.",
            if done {
                "Its commits are"
            } else {
                "Anything it committed is"
            }
        )
    });
    let room = NOTE_MAX.saturating_sub(tail.chars().count());
    (format!("{}{tail}", clip(&text, room)), Some(format!("{text}{tail}")))
}

/// The helper's result from its structured output: the non-empty `result`.
pub fn result_of(structured: &Value) -> Option<String> {
    let r = structured.get("result")?.as_str()?.trim();
    (!r.is_empty()).then(|| r.to_string())
}

#[cfg(test)]
mod tests;
