//! The drafter's structured answer (`DRAFTER_SCHEMA`/`DrafterResult` in Retaskable.com's
//! `agent/src/schemas.ts`, spec 06.5) and the plain-code check on its `breakdown`
//! (plan-breakdown.md section 3.1, `checkedBreakdown` in `shared/planBreakdown.ts`).
use serde::Deserialize;
use serde_json::{Value, json};

pub use crate::implementer::Question;

/// The most lines either half of a breakdown keeps, and each line's longest words (`who`'s).
pub const BREAKDOWN_MAX: usize = 50;
pub const LINE_MAX: usize = 300;
pub const WHO_MAX: usize = 100;
const SIZES: [&str; 4] = ["S", "M", "L", "XL"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrafterStatus {
    PrOpened,
    NeedsClarification,
    Failed,
}

/// One piece the drafter classed as a fix: the agent adds it to the store as a `todo` item.
#[derive(Debug, Clone, Deserialize)]
pub struct Fix {
    pub id: String,
    pub title: String,
    pub remaining: String,
    #[serde(default, rename = "specRefs")]
    pub spec_refs: Vec<String>,
}

/// One piece of the request as the drafter classed it. `kind` is `fix`, `tweak` or `feature`;
/// missing or anything else means it couldn't be classified.
#[derive(Debug, Clone, Deserialize)]
pub struct Piece {
    pub title: String,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DrafterResult {
    pub status: DrafterStatus,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default, rename = "changeTitle")]
    pub change_title: Option<String>,
    #[serde(default, rename = "changeBody")]
    pub change_body: Option<String>,
    #[serde(default, rename = "itemId")]
    pub item_id: Option<String>,
    #[serde(default, rename = "itemIds")]
    pub item_ids: Option<Vec<String>>,
    pub summary: String,
    /// Why a person should read the plan first; empty or left out when nothing in it is risky.
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub questions: Vec<Question>,
    #[serde(default)]
    pub fixes: Vec<Fix>,
    /// Every piece with its kind, when the drafter lists them (`sort_fixes` asks about any it couldn't class).
    #[serde(default)]
    pub pieces: Vec<Piece>,
    /// As the drafter wrote it; [`checked_breakdown`] decides what's kept.
    #[serde(default)]
    pub breakdown: Option<Value>,
    /// The session's own id from the engine, set by `parse_result` (a Cursor chat's id is made by the engine).
    #[serde(skip)]
    pub session_id: Option<String>,
}

impl DrafterResult {
    /// `entryIds` (drafts.ts): every backlog entry it added, in order. A lone `itemId` is a list of one.
    pub fn entry_ids(&self) -> Vec<String> {
        match (&self.item_ids, &self.item_id) {
            (Some(ids), _) => ids.clone(),
            (None, Some(id)) => vec![id.clone()],
            (None, None) => Vec::new(),
        }
    }
}

/// `DRAFTER_SCHEMA` (agent/src/schemas.ts), for a session's `schema`: only `status` and `summary` are required.
pub fn drafter_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": { "enum": ["pr_opened", "needs_clarification", "failed"] },
            "title": { "type": "string" },
            "changeTitle": { "type": "string" },
            "changeBody": { "type": "string" },
            "itemId": { "type": "string" },
            "itemIds": { "type": "array", "items": { "type": "string" } },
            "breakdown": {
                "type": "object",
                "properties": {
                    "build": { "type": "array", "items": {
                        "type": "object",
                        "properties": { "id": { "type": "string" }, "line": { "type": "string" }, "size": { "enum": SIZES } },
                        "required": ["id", "line", "size"]
                    } },
                    "people": { "type": "array", "items": {
                        "type": "object",
                        "properties": { "step": { "type": "string" }, "who": { "type": "string" }, "waitsOn": { "type": "array", "items": { "type": "string" } } },
                        "required": ["step"]
                    } }
                },
                "required": ["build", "people"]
            },
            "fixes": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" }, "title": { "type": "string" }, "remaining": { "type": "string" },
                    "specRefs": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["id", "title", "remaining", "specRefs"]
            } },
            "summary": { "type": "string" },
            "risk": { "type": "string" },
            "questions": { "type": "array", "items": {
                "type": "object",
                "properties": { "header": { "type": "string" }, "question": { "type": "string" }, "options": { "type": "array", "items": { "type": "string" } } },
                "required": ["question", "options"]
            } }
        },
        "required": ["status", "summary"]
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPiece {
    pub id: String,
    pub line: String,
    pub size: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeopleStep {
    pub step: String,
    pub who: Option<String>,
    pub wait_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakdown {
    pub build: Vec<BuildPiece>,
    pub people: Vec<PeopleStep>,
}

fn words(v: Option<&Value>, max: usize) -> String {
    let s = v.and_then(Value::as_str).unwrap_or("");
    crate::text::truncate(&crate::text::squish(s), max)
}

/// The drafter's breakdown, checked: a `build` entry whose id isn't one of `item_ids`, a second entry
/// for the same id, a size outside S to XL (lowercase is read as its capital) or an empty line is
/// dropped; a people step with no words is dropped; a `waitsOn` id that isn't a kept piece is dropped.
/// With no `build` left there is no breakdown. `build` is in `item_ids` order; words are trimmed and
/// cut, and each half keeps at most [`BREAKDOWN_MAX`] lines. Pure.
pub fn checked_breakdown(raw: Option<&Value>, item_ids: &[String]) -> Option<Breakdown> {
    let raw = raw?;
    let mut kept: Vec<BuildPiece> = Vec::new();
    for p in raw.get("build")?.as_array()? {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let line = words(p.get("line"), LINE_MAX);
        let size = p
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_uppercase();
        if !item_ids.contains(&id)
            || kept.iter().any(|k| k.id == id)
            || line.is_empty()
            || !SIZES.contains(&size.as_str())
        {
            continue;
        }
        kept.push(BuildPiece { id, line, size });
    }
    let build: Vec<BuildPiece> = item_ids
        .iter()
        .filter_map(|id| kept.iter().find(|k| &k.id == id).cloned())
        .take(BREAKDOWN_MAX)
        .collect();
    if build.is_empty() {
        return None;
    }
    let people = raw
        .get("people")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let step = words(s.get("step"), LINE_MAX);
            if step.is_empty() {
                return None;
            }
            let who = Some(words(s.get("who"), WHO_MAX)).filter(|w| !w.is_empty());
            let mut wait_on: Vec<String> = Vec::new();
            for id in s
                .get("waitsOn")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                let id = id.trim().to_string();
                if build.iter().any(|b| b.id == id) && !wait_on.contains(&id) {
                    wait_on.push(id);
                }
            }
            Some(PeopleStep { step, who, wait_on })
        })
        .take(BREAKDOWN_MAX)
        .collect();
    Some(Breakdown { build, people })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(v: Value) -> DrafterResult {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn deserializes_a_minimal_result() {
        let r = result(json!({ "status": "failed", "summary": "No" }));
        assert_eq!(r.status, DrafterStatus::Failed);
        assert!(r.questions.is_empty() && r.fixes.is_empty() && r.breakdown.is_none());
    }

    #[test]
    fn deserializes_a_full_result() {
        let r = result(json!({
            "status": "pr_opened", "title": "T", "changeTitle": "Spec: T", "changeBody": "B",
            "itemId": "a", "itemIds": ["a", "b"], "summary": "S",
            "questions": [{ "header": "H", "question": "Q?", "options": ["Fix", "Tweak", "Feature"] }],
            "fixes": [{ "id": "fix-x", "title": "X", "remaining": "R", "specRefs": ["docs/specs/a.md section 1"] }],
            "breakdown": { "build": [], "people": [] }
        }));
        assert_eq!(r.status, DrafterStatus::PrOpened);
        assert_eq!(r.change_title.as_deref(), Some("Spec: T"));
        assert_eq!(r.questions[0].options, ["Fix", "Tweak", "Feature"]);
        assert_eq!(r.fixes[0].spec_refs.len(), 1);
        assert!(r.breakdown.is_some());
    }

    #[test]
    fn the_schema_requires_only_status_and_summary() {
        assert_eq!(drafter_result_schema()["required"], json!(["status", "summary"]));
    }

    #[test]
    fn entry_ids_reads_a_lone_item_id_as_a_list_of_one() {
        let ids = |v| result(v).entry_ids();
        let base = |extra: Value| {
            let mut v = json!({ "status": "pr_opened", "summary": "s" });
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            v
        };
        assert_eq!(ids(base(json!({ "itemIds": ["a", "b"], "itemId": "a" }))), ["a", "b"]);
        assert_eq!(ids(base(json!({ "itemId": "a" }))), ["a"]);
        assert!(ids(base(json!({}))).is_empty());
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_good_breakdown_is_kept_in_item_order() {
        let raw = json!({
            "build": [{ "id": "b", "line": " Two  ", "size": "xl" }, { "id": "a", "line": "One", "size": "S" }],
            "people": [{ "step": "Sign off", "who": "Legal", "waitsOn": ["a", "zzz", "a"] }]
        });
        let b = checked_breakdown(Some(&raw), &ids(&["a", "b"])).unwrap();
        assert_eq!(b.build.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(
            b.build[1],
            BuildPiece {
                id: "b".into(),
                line: "Two".into(),
                size: "XL".into()
            }
        );
        assert_eq!(b.people[0].wait_on, ["a"]);
        assert_eq!(b.people[0].who.as_deref(), Some("Legal"));
    }

    #[test]
    fn the_breakdown_check_rejects_bad_sizes_empty_lines_unknown_and_repeated_ids() {
        let raw = json!({
            "build": [
                { "id": "a", "line": "One", "size": "HUGE" },
                { "id": "b", "line": "  ", "size": "M" },
                { "id": "nope", "line": "Other", "size": "M" },
                { "id": "c", "line": "Three", "size": "M" },
                { "id": "c", "line": "Again", "size": "L" }
            ],
            "people": [{ "step": "  " }, { "step": "Pick a price", "waitsOn": ["a"] }]
        });
        let b = checked_breakdown(Some(&raw), &ids(&["a", "b", "c"])).unwrap();
        assert_eq!(
            b.build,
            [BuildPiece {
                id: "c".into(),
                line: "Three".into(),
                size: "M".into()
            }]
        );
        assert_eq!(b.people.len(), 1);
        assert!(b.people[0].wait_on.is_empty());
        let none = json!({ "build": [{ "id": "a", "line": "", "size": "S" }], "people": [] });
        assert!(checked_breakdown(Some(&none), &ids(&["a"])).is_none());
        assert!(checked_breakdown(None, &ids(&["a"])).is_none());
        assert!(checked_breakdown(Some(&json!({ "people": [] })), &ids(&["a"])).is_none());
    }
}
