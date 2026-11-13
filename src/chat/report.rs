//! The `chat_report` a Chat Agent sends the hub as an ask moves (personal-chat.md section 3.4, askReport.ts): the ask as
//! the hub takes it, its free text cut to the hub's limits. The cloud keeps the chat; the Chat Agent keeps nothing.
use serde_json::{Value, json};

use super::answer::Exchange;
use super::board::Finished;
use crate::text::truncate;

/// The hub cuts a question to this many characters and an answer to [`ANSWER_MAX`] (shared/storeAsks.ts `ASK`).
pub const QUESTION_MAX: usize = 8000;
pub const ANSWER_MAX: usize = 20_000;

/// A message the person sent, as the hub's `ask` command carried it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    /// The command's id: the ask's id everywhere (the row's `askId`, the idea's `clientId`).
    pub id: String,
    pub question: String,
    /// Who asked (a GitHub login), when the hub said.
    pub by: Option<String>,
    pub context: Vec<Exchange>,
    /// No pictures and no reply to an earlier answer: the only kind the classifier sorts.
    pub plain: bool,
    pub reply_to: Option<String>,
    pub about: Option<Value>,
    pub at: String,
}

/// `chat_report` for `ask`: still `thinking` with no `finished`, else `done` or `failed` with its answer.
pub fn chat_report(ask: &Ask, finished: Option<&Finished>) -> Value {
    let mut out = json!({
        "id": ask.id,
        "question": truncate(&ask.question, QUESTION_MAX),
        "state": match finished {
            None => "thinking",
            Some(f) if f.done => "done",
            Some(_) => "failed",
        },
        "outcomes": [],
        "at": ask.at,
    });
    let o = out.as_object_mut().expect("an object");
    if let Some(f) = finished {
        o.insert("answer".into(), json!(truncate(&f.answer, ANSWER_MAX)));
        if let Some(idea) = &f.idea_id {
            o.insert("ideaId".into(), json!(idea));
        }
    }
    if let Some(by) = &ask.by {
        o.insert("by".into(), json!(truncate(by, 100)));
    }
    if let Some(r) = &ask.reply_to {
        o.insert("replyTo".into(), json!(truncate(r, 100)));
    }
    if let Some(about) = &ask.about {
        o.insert("about".into(), about.clone());
    }
    json!({ "type": "chat_report", "ask": out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask() -> Ask {
        Ask {
            id: "c1".into(),
            question: "q".repeat(9000),
            by: Some("ada".into()),
            context: vec![],
            plain: true,
            reply_to: Some("c0".into()),
            about: Some(json!({ "kind": "item", "id": "i1" })),
            at: "2026-11-13T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_new_ask_is_thinking_with_its_question_cut() {
        let m = chat_report(&ask(), None);
        assert_eq!(m["type"], "chat_report");
        let a = &m["ask"];
        assert_eq!(
            (a["state"].clone(), a["outcomes"].clone()),
            (json!("thinking"), json!([]))
        );
        assert_eq!(a["question"].as_str().unwrap().chars().count(), QUESTION_MAX);
        assert_eq!(
            (a["by"].clone(), a["replyTo"].clone(), a["about"]["id"].clone()),
            (json!("ada"), json!("c0"), json!("i1"))
        );
        assert!(a.get("answer").is_none());
    }

    #[test]
    fn a_finished_ask_carries_its_answer_and_the_idea_it_made() {
        let done = Finished {
            idea_id: Some("idea-1".into()),
            ..Finished::done("a".repeat(30_000))
        };
        let a = chat_report(&ask(), Some(&done))["ask"].clone();
        assert_eq!(
            (a["state"].clone(), a["ideaId"].clone()),
            (json!("done"), json!("idea-1"))
        );
        assert_eq!(a["answer"].as_str().unwrap().chars().count(), ANSWER_MAX);
        let failed = chat_report(&ask(), Some(&Finished::failed("no")))["ask"].clone();
        assert_eq!(
            (failed["state"].clone(), failed["answer"].clone()),
            (json!("failed"), json!("no"))
        );
    }
}
