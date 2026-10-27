//! The structured result any schema-checked engine (Cursor, pi) reads from its final message,
//! following `jsonResult.ts`'s `extractJson` and `check.ts`'s `checkSchema` in Retaskable.com (the
//! ground truth this is porting) — real shared files there, used by both engines, so they stay
//! shared here too rather than copied into each engine's own module.
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)```json\s*(.*?)```").unwrap());

/// The last ` ```json ` block, or the last top-level `{…}` that parses. `None` when there's
/// neither (`extractJson`).
pub(super) fn extract_json(text: &str) -> Option<Value> {
    let fences: Vec<&str> = FENCE.captures_iter(text).map(|c| c.get(1).unwrap().as_str()).collect();
    for fence in fences.iter().rev() {
        if let Ok(v) = serde_json::from_str(fence) {
            return Some(v);
        }
    }
    let objects = top_level_objects(text);
    for object in objects.iter().rev() {
        if let Ok(v) = serde_json::from_str(object) {
            return Some(v);
        }
    }
    None
}

/// Balanced `{…}` spans that start outside any other object (`topLevelObjects`).
fn top_level_objects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut start = None;
    let mut quote = false;
    let mut escape = false;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if quote {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                quote = false;
            }
            continue;
        }
        if c == '"' {
            quote = true;
        } else if c == '{' {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if c == '}' && depth > 0 {
            depth -= 1;
            if depth == 0
                && let Some(s) = start
            {
                out.push(chars[s..=i].iter().collect());
            }
        }
    }
    out
}

/// Short lines, like "`status`: must be one of pr_opened, needs_clarification, failed"
/// (`checkSchema`): only the keywords a `--json-schema` result schema uses.
pub(super) fn check_schema(schema: &Value, value: &Value, at: &str) -> Vec<String> {
    if !schema.is_object() {
        return Vec::new();
    }
    let where_at = if at.is_empty() { "result" } else { at };
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.contains(value)
    {
        let names: Vec<String> = enum_values.iter().map(display_value).collect();
        return vec![format!("{where_at}: must be one of {}", names.join(", "))];
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(obj) = value.as_object() else {
                return vec![format!("{where_at}: must be an object")];
            };
            let mut errors = Vec::new();
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(key) {
                        let key_at = if at.is_empty() {
                            key.to_string()
                        } else {
                            format!("{at}.{key}")
                        };
                        errors.push(format!("{key_at}: is required"));
                    }
                }
            }
            let no_props = json!({});
            let props = schema.get("properties").unwrap_or(&no_props);
            for (key, child) in obj {
                let child_at = if at.is_empty() {
                    key.clone()
                } else {
                    format!("{at}.{key}")
                };
                if let Some(child_schema) = props.get(key) {
                    errors.extend(check_schema(child_schema, child, &child_at));
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    errors.push(format!("{child_at}: is not allowed"));
                }
            }
            errors
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                return vec![format!("{where_at}: must be an array")];
            };
            match schema.get("items") {
                Some(item_schema) => items
                    .iter()
                    .enumerate()
                    .flat_map(|(i, item)| check_schema(item_schema, item, &format!("{where_at}[{i}]")))
                    .collect(),
                None => Vec::new(),
            }
        }
        Some("string") if !value.is_string() => vec![format!("{where_at}: must be a string")],
        Some("number") if !value.is_number() => vec![format!("{where_at}: must be a number")],
        Some("boolean") if !value.is_boolean() => vec![format!("{where_at}: must be a boolean")],
        _ => Vec::new(),
    }
}

fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // extract_json

    #[test]
    fn extract_json_takes_the_last_fenced_block_then_the_last_top_level_object_then_none() {
        assert_eq!(
            extract_json("notes\n```json\n{\"ok\":false}\n```\nthen\n```json\n{\"ok\":true}\n```\nthanks"),
            Some(json!({"ok": true}))
        );
        assert_eq!(
            extract_json("no fence, just {\"ok\":true} at the end"),
            Some(json!({"ok": true}))
        );
        assert_eq!(extract_json("nothing here"), None);
    }

    // check_schema

    #[test]
    fn check_schema_names_an_enum_miss_and_a_required_miss() {
        let schema = json!({
            "type": "object",
            "properties": {"status": {"enum": ["pr_opened", "failed"]}},
            "required": ["status"],
        });
        assert_eq!(
            check_schema(&schema, &json!({"status": "pr_opened"}), ""),
            Vec::<String>::new()
        );
        assert_eq!(
            check_schema(&schema, &json!({"status": "nope"}), ""),
            vec!["status: must be one of pr_opened, failed".to_string()]
        );
        assert_eq!(
            check_schema(&schema, &json!({}), ""),
            vec!["status: is required".to_string()]
        );
    }

    #[test]
    fn check_schema_names_a_type_mismatch_and_disallows_extras_when_asked() {
        let schema = json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "additionalProperties": false,
        });
        let errors = check_schema(&schema, &json!({"ok": "yes"}), "");
        assert!(errors[0].contains("ok: must be a boolean"), "{errors:?}");
        let errors = check_schema(&schema, &json!({"ok": true, "extra": 1}), "");
        assert!(errors.contains(&"extra: is not allowed".to_string()));
    }
}
