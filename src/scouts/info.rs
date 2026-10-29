//! Each scout's focus and instructions (`prompts/scouts.json`, Retaskable.com's `scoutInfo` in
//! `agent/src/review.ts`), looked up by [`super::plan::ScoutKind`].
use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::plan::ScoutKind;

/// One scout's entry in `prompts/scouts.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ScoutInfo {
    pub focus: String,
    pub instructions: String,
}

/// Parses `prompts/scouts.json`'s content into a lookup by scout name.
pub fn parse_scout_info(json: &str) -> Result<HashMap<String, ScoutInfo>> {
    serde_json::from_str(json).context("parsing scouts.json")
}

/// `info`'s entry for `kind`, or an error naming it when `prompts/scouts.json` doesn't have one.
pub fn scout_info(info: &HashMap<String, ScoutInfo>, kind: ScoutKind) -> Result<&ScoutInfo> {
    info.get(kind.as_str())
        .with_context(|| format!("prompts/scouts.json has no entry for {}", kind.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
        "correctness": { "focus": "correctness", "instructions": "look for bugs" },
        "tests": { "focus": "tests", "instructions": "look for missing tests" }
    }"#;

    #[test]
    fn parse_scout_info_reads_every_entry() {
        let info = parse_scout_info(JSON).unwrap();
        assert_eq!(info.len(), 2);
        assert_eq!(scout_info(&info, ScoutKind::Correctness).unwrap().focus, "correctness");
    }

    #[test]
    fn scout_info_errors_on_a_kind_the_json_has_no_entry_for() {
        let info = parse_scout_info(JSON).unwrap();
        let err = scout_info(&info, ScoutKind::Spec).unwrap_err();
        assert!(err.to_string().contains("spec"));
    }

    #[test]
    fn the_real_scouts_json_has_every_scout_kind() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("prompts")
            .join("scouts.json");
        let json = std::fs::read_to_string(path).unwrap();
        let info = parse_scout_info(&json).unwrap();
        for kind in [ScoutKind::Correctness, ScoutKind::Tests, ScoutKind::Spec] {
            let entry = scout_info(&info, kind).unwrap();
            assert!(!entry.focus.is_empty());
            assert!(!entry.instructions.is_empty());
        }
    }
}
