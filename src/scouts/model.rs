//! A scout session's structured result (`SCOUT_SCHEMA`/`ScoutResult`, Retaskable.com's
//! `agent/src/schemas.ts`).
use serde::Deserialize;
use serde_json::{Value, json};

/// One problem a scout found, each backed by evidence (`Finding`, `agent/src/schemas.ts`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Finding {
    pub severity: String,
    pub file: String,
    pub line: i64,
    pub evidence: String,
    pub body: String,
}

/// A scout's full answer (`ScoutResult`, `agent/src/schemas.ts`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScoutResult {
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub notes: String,
}

/// `SCOUT_RESULT_SCHEMA` (06.3-review.md), for a scout session's `schema`.
pub fn scout_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "enum": ["blocker", "major", "minor"] },
                        "file": { "type": "string" },
                        "line": { "type": "number" },
                        "evidence": { "type": "string" },
                        "body": { "type": "string" }
                    },
                    "required": ["severity", "file", "line", "evidence", "body"]
                }
            },
            "notes": { "type": "string" }
        },
        "required": ["findings", "notes"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scout_result_parses_from_the_schemas_own_shape() {
        let v = json!({
            "findings": [{
                "severity": "major",
                "file": "src/x.rs",
                "line": 12,
                "evidence": "let x = 1;",
                "body": "unused",
            }],
            "notes": "looks fine otherwise",
        });
        let res: ScoutResult = serde_json::from_value(v).unwrap();
        assert_eq!(res.findings.len(), 1);
        assert_eq!(res.findings[0].severity, "major");
        assert_eq!(res.notes, "looks fine otherwise");
    }

    #[test]
    fn scout_result_defaults_missing_findings_and_notes() {
        let res: ScoutResult = serde_json::from_value(json!({})).unwrap();
        assert!(res.findings.is_empty());
        assert_eq!(res.notes, "");
    }
}
