//! The assistant's result `{ answer, actions: [...] }` and the fixed list of actions it may take
//! (spec 06.6). Pure code: an action that is unknown or malformed is dropped and its reason kept,
//! so the orchestrator can report it with the answer.
use serde_json::{Value, json};

/// One action the orchestrator will run through the same code as the command a person could send.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Lane 0 is the base build.
    Play {
        lane: u32,
    },
    Stop {
        lane: u32,
    },
    Pause,
    Resume,
    Retry {
        id: String,
    },
    Skip {
        id: String,
    },
    DraftSpec {
        request: String,
        titles: Vec<String>,
        /// One theme per title, or empty.
        themes: Vec<String>,
        audience: Option<String>,
        revises: Option<String>,
    },
}

/// What the assistant returned, with its actions checked.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantResult {
    pub answer: String,
    pub actions: Vec<Action>,
    /// Why each dropped action was dropped.
    pub dropped: Vec<String>,
}

pub fn assistant_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" },
            "actions": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "name": { "enum": ["play", "stop", "pause", "resume", "retry", "skip", "draft_spec"] },
                    "lane": { "type": "integer", "minimum": 0 },
                    "id": { "type": "string" },
                    "request": { "type": "string" },
                    "titles": { "type": "array", "items": { "type": "string" } },
                    "themes": { "type": "array", "items": { "type": "string" } },
                    "audience": { "type": ["string", "null"] },
                    "revises": { "type": ["string", "null"] }
                },
                "required": ["name"]
            } }
        },
        "required": ["answer", "actions"]
    })
}

fn text<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

fn id_or_null(v: &Value, key: &str) -> Option<String> {
    text(v, key).map(|s| s.trim().to_string())
}

/// Checks one action; `Err` is the reason it was dropped.
pub fn to_action(v: &Value) -> Result<Action, String> {
    let name = v.get("name").and_then(Value::as_str).ok_or("an action has no name")?;
    let lane = || {
        v.get("lane")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("{name}: lane must be a whole number, 0 or more"))
    };
    let id = || id_or_null(v, "id").ok_or_else(|| format!("{name}: needs an item id"));
    match name {
        "play" => Ok(Action::Play { lane: lane()? }),
        "stop" => Ok(Action::Stop { lane: lane()? }),
        "pause" => Ok(Action::Pause),
        "resume" => Ok(Action::Resume),
        "retry" => Ok(Action::Retry { id: id()? }),
        "skip" => Ok(Action::Skip { id: id()? }),
        "draft_spec" => {
            let request = text(v, "request").ok_or("draft_spec: needs a request")?.to_string();
            let titles: Vec<String> = strings(v.get("titles"))
                .into_iter()
                .filter_map(|t| t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()))
                .collect();
            let themes: Option<Vec<String>> = strings(v.get("themes")).into_iter().collect();
            let themes = themes.filter(|t| t.len() == titles.len()).unwrap_or_default();
            Ok(Action::DraftSpec {
                request,
                titles,
                themes,
                audience: id_or_null(v, "audience"),
                revises: id_or_null(v, "revises"),
            })
        }
        other => Err(format!("unknown action \"{other}\"")),
    }
}

/// Each array element as a string, or `None` where it is not one.
fn strings(v: Option<&Value>) -> Vec<Option<String>> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Reads the assistant's result; `None` when it has no answer.
pub fn parse_assistant_result(v: &Value) -> Option<AssistantResult> {
    let answer = v.get("answer")?.as_str()?.to_string();
    let (mut actions, mut dropped) = (vec![], vec![]);
    for a in v.get("actions").and_then(Value::as_array).into_iter().flatten() {
        match to_action(a) {
            Ok(a) => actions.push(a),
            Err(why) => dropped.push(why),
        }
    }
    Some(AssistantResult {
        answer,
        actions,
        dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(v: Value) -> Action {
        to_action(&v).unwrap()
    }
    fn why(v: Value) -> String {
        to_action(&v).unwrap_err()
    }

    #[test]
    fn simple_actions() {
        assert_eq!(ok(json!({ "name": "play", "lane": 0 })), Action::Play { lane: 0 });
        assert_eq!(ok(json!({ "name": "stop", "lane": 2 })), Action::Stop { lane: 2 });
        assert_eq!(ok(json!({ "name": "pause" })), Action::Pause);
        assert_eq!(ok(json!({ "name": "resume" })), Action::Resume);
        assert_eq!(
            ok(json!({ "name": "retry", "id": "a" })),
            Action::Retry { id: "a".into() }
        );
        assert_eq!(
            ok(json!({ "name": "skip", "id": "b" })),
            Action::Skip { id: "b".into() }
        );
    }

    #[test]
    fn bad_lanes_and_ids_are_dropped() {
        assert!(why(json!({ "name": "play", "lane": "1" })).contains("lane"));
        assert!(why(json!({ "name": "stop", "lane": -1 })).contains("lane"));
        assert!(why(json!({ "name": "play" })).contains("lane"));
        assert!(why(json!({ "name": "retry", "id": " " })).contains("id"));
        assert!(why(json!({ "name": "skip", "id": 3 })).contains("id"));
    }

    #[test]
    fn unknown_and_nameless_actions_are_dropped() {
        assert!(why(json!({ "name": "deploy" })).contains("deploy"));
        assert!(why(json!({ "lane": 1 })).contains("name"));
        assert!(why(json!("play")).contains("name"));
    }

    fn draft(extra: Value) -> Action {
        let mut v = json!({ "name": "draft_spec", "request": "Add dark mode" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ok(v)
    }
    fn parts(a: Action) -> (Vec<String>, Vec<String>) {
        match a {
            Action::DraftSpec { titles, themes, .. } => (titles, themes),
            _ => panic!(),
        }
    }

    #[test]
    fn draft_spec_needs_a_request() {
        assert!(why(json!({ "name": "draft_spec" })).contains("request"));
        assert!(why(json!({ "name": "draft_spec", "request": "  " })).contains("request"));
    }

    #[test]
    fn titles_keep_non_empty_strings_in_order() {
        let (t, th) = parts(draft(json!({ "titles": ["B", "", "  ", 4, null, "A"] })));
        assert_eq!(t, ["B", "A"]);
        assert!(th.is_empty());
    }

    #[test]
    fn themes_are_kept_only_one_per_title() {
        let (t, th) = parts(draft(json!({ "titles": ["a", "b"], "themes": ["x", "y"] })));
        assert_eq!((t.len(), th), (2, vec!["x".to_string(), "y".to_string()]));
        assert!(
            parts(draft(json!({ "titles": ["a", "b"], "themes": ["x"] })))
                .1
                .is_empty()
        );
        assert!(
            parts(draft(json!({ "titles": ["a"], "themes": ["x", "y"] })))
                .1
                .is_empty()
        );
        assert!(
            parts(draft(json!({ "titles": ["a", "b"], "themes": ["x", 1] })))
                .1
                .is_empty()
        );
        // Blank titles are dropped first, so the themes must match the kept ones.
        assert_eq!(parts(draft(json!({ "titles": ["a", ""], "themes": ["x"] }))).1, ["x"]);
    }

    #[test]
    fn audience_and_revises_are_ids_or_null() {
        let Action::DraftSpec { audience, revises, .. } = draft(json!({ "audience": "staff", "revises": "item-1" }))
        else {
            panic!()
        };
        assert_eq!(
            (audience.as_deref(), revises.as_deref()),
            (Some("staff"), Some("item-1"))
        );
        let Action::DraftSpec { audience, revises, .. } = draft(json!({ "audience": null, "revises": 5 })) else {
            panic!()
        };
        assert_eq!((audience, revises), (None, None));
        let Action::DraftSpec { audience, revises, .. } = draft(json!({})) else {
            panic!()
        };
        assert_eq!((audience, revises), (None, None));
    }

    #[test]
    fn a_result_parses_from_the_schemas_shape() {
        let raw = json!({
            "answer": "On it.",
            "actions": [
                { "name": "pause" },
                { "name": "dance" },
                { "name": "draft_spec", "request": "r", "titles": ["T"], "themes": ["Th"], "audience": null, "revises": null }
            ]
        });
        let required = assistant_result_schema()["required"].clone();
        assert!(
            required
                .as_array()
                .unwrap()
                .iter()
                .all(|k| raw.get(k.as_str().unwrap()).is_some())
        );
        let r = parse_assistant_result(&raw).unwrap();
        assert_eq!(r.answer, "On it.");
        assert_eq!(r.actions.len(), 2);
        assert_eq!(r.dropped.len(), 1);
        assert!(parse_assistant_result(&json!({ "actions": [] })).is_none());
    }
}
