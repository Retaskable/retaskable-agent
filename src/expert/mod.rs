//! An expert session's own rules, with nothing of the store, the lanes or the engine in them
//! (docs/specs/expert-chat.md; Retaskable.com's `agent/src/expert/session.ts` and `turns.ts`): who may drive it, how
//! a session is titled, named and branched, the prompt a fresh session starts from, the schema each turn ends with and
//! how that result is read back. The session itself runs in `orchestrator::driver::experts`.
use crate::text::truncate;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::draft_worktree::slug;
use crate::prompt::render_prompt;

/// `prompts/expert.md`.
const TEMPLATE: &str = include_str!("../../prompts/expert.md");

/// How long an idle session keeps its lane (`expertIdleMinutes`' default).
pub const IDLE: Duration = Duration::from_secs(30 * 60);
/// How many ids `expert-<slug>`, `expert-<slug>-2`, ... a new session tries before giving up.
pub const ID_TRIES: u32 = 20;
/// What a turn's answer is cut to.
const ANSWER_MAX: usize = 20_000;
/// The most suggested answers a question shows.
const OPTIONS_MAX: usize = 4;
/// What a stopped turn's message begins with when the person's next one goes first.
pub const MID_STEP: &str = "The person stopped you mid-step, so your last step may not have finished. Their message:";

/// What the session is doing, as the item's `expert.detail` says it (`expert_working` and the others).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Working,
    Asking,
    Idle,
    Parked,
}

impl Detail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "expert_working",
            Self::Asking => "expert_asking",
            Self::Idle => "expert_idle",
            Self::Parked => "expert_parked",
        }
    }
}

/// Why `by` may not drive a session `started_by` started, or `None` when they may (`driverRefusal`).
pub fn driver_refusal(started_by: &str, by: &str) -> Option<String> {
    (started_by != by).then(|| format!("Only {started_by}, who started this session, can drive it."))
}

/// A session's title: its request's first line, cut at a word to 80 characters.
pub fn title(request: &str) -> String {
    let line = request.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() <= 80 {
        return line.to_string();
    }
    let cut: String = line.chars().take(80).collect();
    let at = match cut.rfind(' ') {
        Some(i) if cut[..i].chars().count() > 40 => i,
        _ => cut.len(),
    };
    format!("{}…", cut[..at].trim_end())
}

/// The id a session of `title` tries on its `n`th attempt: `expert-<slug>`, then `expert-<slug>-2` and on.
pub fn id_for(title: &str, n: u32) -> String {
    let base = format!("expert-{}", slug(title));
    if n <= 1 { base } else { format!("{base}-{n}") }
}

/// Whether an item of the store is an expert session: the loop leaves those to this module.
pub fn is_expert(id: &str, kind: Option<&str>) -> bool {
    kind == Some("expert") || id.starts_with("expert-")
}

/// Its branch: `retaskable/<id>-<slug>`, one per session.
pub fn branch(id: &str, title: &str) -> String {
    format!("retaskable/{id}-{}", slug(title))
}

/// What a fresh session is told before the person's first message.
pub fn first_prompt(branch: &str, base_branch: &str, message: &str) -> Result<String> {
    let head = render_prompt(
        TEMPLATE,
        &[
            ("branch", branch.to_string()),
            ("baseBranch", base_branch.to_string()),
            (
                "answers",
                "always with 2–4 suggested answers, the one you recommend first and the safe, sensible choice".into(),
            ),
        ],
    )?;
    Ok(format!("{head}\n\n{message}"))
}

/// The message with its pictures' block after it (`attachments.withImages`): no block, the message as it is.
pub fn with_images(text: &str, block: &str) -> String {
    if block.is_empty() {
        return text.to_string();
    }
    let sep = if text.trim().is_empty() { "" } else { "\n\n" };
    format!("{}{sep}{block}", text.trim_end())
}

/// The structured result each turn ends with.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string", "description": "Your reply to the person for this turn, in Markdown: what you did and what they need to know. Short when that says it." },
            "question": {
                "type": "object",
                "description": "Only when you need a decision from the person before going on: what to ask them. Their answer is the next message.",
                "properties": {
                    "question": { "type": "string" },
                    "options": {
                        "type": "array",
                        "description": "The answers you'd suggest, up to four, the one you recommend first. Empty when only their own words will do.",
                        "items": {
                            "type": "object",
                            "properties": { "label": { "type": "string" }, "detail": { "type": "string" } },
                            "required": ["label"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["question", "options"],
                "additionalProperties": false
            }
        },
        "required": ["summary"],
        "additionalProperties": false
    })
}

/// A question a turn ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    pub options: Vec<(String, Option<String>)>,
}

impl Question {
    pub fn to_json(&self) -> Value {
        let options: Vec<Value> = (self.options.iter())
            .map(|(label, detail)| match detail {
                Some(d) => json!({ "label": label, "detail": d }),
                None => json!({ "label": label }),
            })
            .collect();
        json!({ "question": self.question, "options": options })
    }
}

/// What a finished turn said.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reply {
    pub summary: String,
    pub ask: Option<Question>,
}

/// Reads a turn's structured result (`askOf` and the summary of `settle`): the summary cut to size, and a question
/// only when it has words, with at most four options that have a label.
pub fn parse(structured: &Value) -> Reply {
    let summary =
        (structured.get("summary").and_then(Value::as_str)).map_or(String::new(), |s| truncate(s.trim(), ANSWER_MAX));
    let ask = structured.get("question").and_then(|q| {
        let text = q.get("question")?.as_str()?.trim();
        if text.is_empty() {
            return None;
        }
        let options = (q.get("options").and_then(Value::as_array).into_iter().flatten())
            .filter_map(|o| {
                let label = o.get("label")?.as_str()?.trim();
                let detail = o
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|d| !d.is_empty());
                (!label.is_empty()).then(|| (truncate(label, 120), detail.map(|d| truncate(d, 300))))
            })
            .take(OPTIONS_MAX)
            .collect();
        Some(Question {
            question: truncate(text, 2000),
            options,
        })
    });
    Reply { summary, ask }
}

/// The line the item's history gets for a turn's answer (`settle`): the summary, then what it asks.
pub fn answer_line(reply: &Reply) -> String {
    let asks = reply
        .ask
        .as_ref()
        .map(|q| format!("Asks: {}", truncate(&q.question, 300)));
    [Some(reply.summary.clone()).filter(|s| !s.is_empty()), asks]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
