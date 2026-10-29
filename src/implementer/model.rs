//! The implementer session's structured answer (`IMPLEMENTER_SCHEMA`/`ImplementerResult` in
//! Retaskable.com's `agent/src/schemas.ts`), ported here with every field the ground truth has,
//! including `waitsOn` (section 6.2's "Waits on another item"), which the local spec's schema had
//! dropped before this slice's first commit fixed it.
use serde::Deserialize;
use serde_json::{Value, json};

/// `playtest` (schemas.ts): what to look at and the steps to get there, when a slice changes
/// something a project's user can see or feel.
#[derive(Debug, Clone, Deserialize)]
pub struct Playtest {
    pub what: String,
    #[serde(default)]
    pub steps: Vec<String>,
}

/// One entry of `questions` (schemas.ts): a short, self-contained question with suggested options.
#[derive(Debug, Clone, Deserialize)]
pub struct Question {
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

/// `ImplementerResult` (agent/src/schemas.ts): the structured answer an implementer session
/// returns. `status` stays a `String` here (not an enum) so an unrecognized value a future prompt
/// adds doesn't fail to deserialize; callers match on it the way `steps.ts` does.
#[derive(Debug, Clone, Deserialize)]
pub struct ImplementerResult {
    pub status: String,
    #[serde(default, rename = "personSteps")]
    pub person_steps: Vec<String>,
    /// `waits_on`: the ids of the backlog items this item needs built first.
    #[serde(default, rename = "waitsOn")]
    pub waits_on: Vec<String>,
    #[serde(default, rename = "changeTitle")]
    pub change_title: Option<String>,
    #[serde(default, rename = "changeBody")]
    pub change_body: Option<String>,
    pub summary: String,
    #[serde(default, rename = "sliceDescription")]
    pub slice_description: String,
    #[serde(default, rename = "specWasStale")]
    pub spec_was_stale: bool,
    #[serde(default, rename = "specNotes")]
    pub spec_notes: Option<String>,
    #[serde(default, rename = "itemDone")]
    pub item_done: Option<bool>,
    #[serde(default)]
    pub remaining: Option<String>,
    #[serde(default)]
    pub playtest: Option<Playtest>,
    #[serde(default)]
    pub questions: Vec<Question>,
}

/// `IMPLEMENTER_SCHEMA` (agent/src/schemas.ts), for a session's `schema`.
pub fn implementer_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": { "enum": ["pr_opened", "needs_clarification", "failed", "nothing_left", "person_steps", "waits_on"] },
            "personSteps": { "type": "array", "items": { "type": "string" } },
            "waitsOn": { "type": "array", "items": { "type": "string" } },
            "changeTitle": { "type": "string" },
            "changeBody": { "type": "string" },
            "summary": { "type": "string" },
            "sliceDescription": { "type": "string" },
            "specWasStale": { "type": "boolean" },
            "specNotes": { "type": "string" },
            "itemDone": { "type": "boolean" },
            "remaining": { "type": "string" },
            "playtest": {
                "type": "object",
                "properties": {
                    "what": { "type": "string" },
                    "steps": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["what", "steps"]
            },
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "header": { "type": "string" },
                        "question": { "type": "string" },
                        "options": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["question", "options"]
                }
            }
        },
        "required": ["status", "summary"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_a_minimal_result() {
        let r: ImplementerResult = serde_json::from_value(json!({
            "status": "pr_opened",
            "summary": "Did a thing",
        }))
        .unwrap();
        assert_eq!(r.status, "pr_opened");
        assert_eq!(r.summary, "Did a thing");
        assert!(r.person_steps.is_empty());
        assert!(r.waits_on.is_empty());
        assert!(!r.spec_was_stale);
        assert!(r.playtest.is_none());
    }

    #[test]
    fn deserializes_every_field_including_waits_on() {
        let r: ImplementerResult = serde_json::from_value(json!({
            "status": "waits_on",
            "waitsOn": ["store", "auth"],
            "personSteps": ["Open the page."],
            "changeTitle": "item-1: Title",
            "changeBody": "Body",
            "summary": "Needs store and auth built first",
            "sliceDescription": "n/a",
            "specWasStale": true,
            "specNotes": "Updated the spec",
            "itemDone": false,
            "remaining": "The rest",
            "playtest": { "what": "Look at it", "steps": ["Open it"] },
            "questions": [{ "header": "H", "question": "Which?", "options": ["A", "B"] }],
        }))
        .unwrap();
        assert_eq!(r.waits_on, vec!["store", "auth"]);
        assert!(r.spec_was_stale);
        assert_eq!(r.spec_notes.as_deref(), Some("Updated the spec"));
        assert_eq!(r.item_done, Some(false));
        assert_eq!(r.playtest.unwrap().steps, vec!["Open it"]);
        assert_eq!(r.questions[0].header, "H");
    }

    #[test]
    fn schema_requires_only_status_and_summary_and_lists_waits_on() {
        let schema = implementer_result_schema();
        assert_eq!(schema["required"], json!(["status", "summary"]));
        assert_eq!(
            schema["properties"]["status"]["enum"],
            json!([
                "pr_opened",
                "needs_clarification",
                "failed",
                "nothing_left",
                "person_steps",
                "waits_on"
            ])
        );
        assert_eq!(schema["properties"]["waitsOn"]["type"], json!("array"));
        assert_eq!(schema["properties"]["specWasStale"]["type"], json!("boolean"));
    }
}
