//! The pure half of the `plan` run (docs/specs/pieces.md section 3.2): the planner's prompt, the schema of its
//! structured result, and reading that result into the `planned` result for `run_done`.
use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::backlog::runs::RunPiece;
use crate::prompt::render_prompt;

const TEMPLATE: &str = include_str!("../../prompts/planner.md");

/// The most pieces a plan may have.
pub const MAX_PIECES: usize = 30;

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// The item's specs from the brief: a list of strings, or one string.
fn specs(piece: &RunPiece) -> String {
    let list: Vec<String> = match piece.other.get("specs") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => vec![],
    };
    let list: Vec<String> = list
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if list.is_empty() {
        "none (find them in `docs/specs`)".into()
    } else {
        list.join(", ")
    }
}

fn attempts_section(piece: &RunPiece) -> String {
    let lines: Vec<String> = piece
        .attempts
        .iter()
        .map(|a| format!("- the {} {}: {}", text(a, "kind"), text(a, "outcome"), text(a, "why")))
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "## Earlier tries at this plan\n\nThese runs did not give a plan that worked. Don't repeat what they got wrong:\n\n{}\n\n",
        lines.join("\n")
    )
}

fn look_section(piece: &RunPiece) -> String {
    piece
        .other
        .get("looks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .filter_map(|l| l.get("instructions").and_then(Value::as_str))
        .map(str::trim)
        .find(|i| !i.is_empty())
        .map(|i| format!("## What a look said\n\nA look at the earlier tries told this one:\n\n{i}\n\n"))
        .unwrap_or_default()
}

/// Fills `prompts/planner.md` from the run's brief. An unfilled `{{…}}` is an error.
pub fn build_plan_prompt(piece: &RunPiece) -> Result<String, String> {
    let what = piece.what.trim();
    let what_line = if what.is_empty() {
        String::new()
    } else {
        format!("- What is wanted: {what}\n")
    };
    let vars = [
        ("title", piece.title.trim().to_string()),
        ("whatLine", what_line),
        ("specs", specs(piece)),
        (
            "briefSection",
            format!("{}{}", attempts_section(piece), look_section(piece)),
        ),
    ];
    let out = render_prompt(TEMPLATE, &vars).map_err(|e| format!("{e:#}"))?;
    // A value that itself carries a placeholder would also be an unfilled prompt variable.
    if out.contains("{{") {
        return Err("the planner prompt has an unfilled {{…}} placeholder".into());
    }
    Ok(out)
}

/// For `SessionSpec.schema`.
pub fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "pieces": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_PIECES,
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "what": { "type": "string" },
                        "doneWhen": { "type": "string" },
                        "needs": { "type": "array", "items": { "type": "integer" } }
                    },
                    "required": ["title", "what", "doneWhen", "needs"]
                }
            },
            "noFlag": { "type": "string" }
        },
        "required": ["pieces"]
    })
}

/// Checks the session's result and turns it into `run_done`'s `planned` result, `needs` as the needed pieces'
/// titles. `Err` is a sentence a person can read.
pub fn read_plan(structured: &Value) -> Result<Value, String> {
    let pieces = structured
        .get("pieces")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())
        .ok_or("The plan has no pieces.")?;
    if pieces.len() > MAX_PIECES {
        return Err(format!(
            "The plan has {} pieces; the most is {MAX_PIECES}.",
            pieces.len()
        ));
    }
    let mut titles: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for (i, p) in pieces.iter().enumerate() {
        let n = i + 1;
        let title = text(p, "title").trim().to_string();
        if title.is_empty() {
            return Err(format!("Piece {n} has no title."));
        }
        for field in ["what", "doneWhen"] {
            if text(p, field).trim().is_empty() {
                return Err(format!("Piece {n} (\"{title}\") has an empty {field}."));
            }
        }
        if !seen.insert(title.to_lowercase()) {
            return Err(format!("Two pieces are both called \"{title}\"."));
        }
        titles.push(title);
    }
    let mut out = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        let n = i + 1;
        let mut needs = Vec::new();
        for need in p.get("needs").and_then(Value::as_array).into_iter().flatten() {
            match need.as_u64() {
                Some(k) if k >= 1 && (k as usize) < n => needs.push(Value::String(titles[k as usize - 1].clone())),
                Some(k) if k as usize == n => {
                    return Err(format!("Piece {n} (\"{}\") needs itself.", titles[i]));
                }
                Some(k) if k as usize > n && k as usize <= titles.len() => {
                    return Err(format!(
                        "Piece {n} (\"{}\") needs piece {k}, which comes later.",
                        titles[i]
                    ));
                }
                _ => {
                    return Err(format!(
                        "Piece {n} (\"{}\") needs {need}, which is not a piece of the plan (1 to {}).",
                        titles[i],
                        titles.len()
                    ));
                }
            }
        }
        out.push(json!({
            "title": titles[i],
            "what": text(p, "what").trim(),
            "doneWhen": text(p, "doneWhen").trim(),
            "needs": needs,
        }));
    }
    let mut result = Map::new();
    result.insert("pieces".into(), Value::Array(out));
    let no_flag = text(structured, "noFlag");
    if !no_flag.trim().is_empty() {
        result.insert("noFlag".into(), Value::String(no_flag.trim().to_string()));
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief(extra: Value) -> RunPiece {
        let mut v = json!({ "title": "Add the widget", "specs": ["docs/specs/widget.md", "docs/specs/a.md"] });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn prompt_has_title_and_specs_and_no_placeholder() {
        let p = build_plan_prompt(&brief(json!({}))).unwrap();
        assert!(p.contains("Add the widget") && p.contains("docs/specs/widget.md, docs/specs/a.md"));
        assert!(!p.contains("{{") && !p.contains("Earlier tries") && !p.contains("What a look said"));
        assert!(p.contains("noFlag") && p.contains("doneWhen") && p.contains("at most 30") | p.contains("At most 30"));
    }

    #[test]
    fn prompt_takes_a_string_spec_and_what() {
        let p = build_plan_prompt(&brief(json!({ "specs": "docs/specs/one.md", "what": "Do it" }))).unwrap();
        assert!(p.contains("docs/specs/one.md") && p.contains("What is wanted: Do it"));
    }

    #[test]
    fn prompt_includes_attempts_and_the_latest_look() {
        let p = build_plan_prompt(&brief(json!({
            "attempts": [{ "kind": "plan", "outcome": "failed", "why": "too big" }],
            "looks": [{ "instructions": "STALE-ADVICE" }, { "instructions": "  Split by spec section.  " }, {}]
        })))
        .unwrap();
        assert!(p.contains("- the plan failed: too big"));
        assert!(p.contains("Split by spec section.") && !p.contains("STALE-ADVICE"));
        assert!(!p.contains("{{"));
    }

    #[test]
    fn prompt_errors_on_an_unfilled_placeholder() {
        let e = build_plan_prompt(&brief(json!({ "what": "use {{thing}}" }))).unwrap_err();
        assert!(e.contains("{{"));
    }

    #[test]
    fn schema_requires_the_four_piece_fields() {
        let s = plan_schema();
        let item = &s["properties"]["pieces"]["items"];
        assert_eq!(item["required"], json!(["title", "what", "doneWhen", "needs"]));
        assert_eq!(s["properties"]["pieces"]["maxItems"], 30);
        assert_eq!(s["required"], json!(["pieces"]));
    }

    fn piece(title: &str, needs: Value) -> Value {
        json!({ "title": title, "what": " w ", "doneWhen": "d", "needs": needs })
    }

    #[test]
    fn reads_a_good_plan_with_needs_as_titles() {
        let r = read_plan(&json!({
            "pieces": [piece("A", json!([])), piece("B", json!([1])), piece("C", json!([1, 2]))],
            "noFlag": "  internal only "
        }))
        .unwrap();
        assert_eq!(
            r["pieces"][0],
            json!({ "title": "A", "what": "w", "doneWhen": "d", "needs": [] })
        );
        assert_eq!(r["pieces"][2]["needs"], json!(["A", "B"]));
        assert_eq!(r["noFlag"], "internal only");
        let r = read_plan(&json!({ "pieces": [piece("A", json!([]))], "noFlag": "  " })).unwrap();
        assert!(r.get("noFlag").is_none());
    }

    #[test]
    fn refuses_bad_plans_with_a_reason() {
        let err = |v: Value| read_plan(&v).unwrap_err();
        assert!(err(json!({ "pieces": [] })).contains("no pieces"));
        assert!(err(json!({})).contains("no pieces"));
        let many: Vec<Value> = (0..31).map(|i| piece(&format!("P{i}"), json!([]))).collect();
        assert!(err(json!({ "pieces": many })).contains("31"));
        let mut empty = piece("A", json!([]));
        empty["what"] = json!("  ");
        assert!(err(json!({ "pieces": [empty] })).contains("empty what"));
        assert!(err(json!({ "pieces": [piece("A", json!([])), piece("A", json!([]))] })).contains("both called"));
        assert!(err(json!({ "pieces": [piece("A", json!([1]))] })).contains("itself"));
        assert!(err(json!({ "pieces": [piece("A", json!([2])), piece("B", json!([]))] })).contains("later"));
        assert!(err(json!({ "pieces": [piece("A", json!([0]))] })).contains("not a piece"));
        assert!(err(json!({ "pieces": [piece("A", json!([9]))] })).contains("not a piece"));
    }
}
