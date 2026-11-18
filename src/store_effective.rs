//! The settings the store holds a default for (`SETTING_DEFAULTS` in Retaskable.com's `shared/settingDefaults.ts`, pools-on-server.md
//! section 18.5), as `settings_get` answers them in `effective`: each the project's saved value or the store's default. The agent holds
//! none of its own: until it has read them nothing that needs one starts (fail closed), and a value a file carries is never used.
use serde::Deserialize;
use serde_json::Value;

use crate::config::{CodeGates, ProjectConfig, ReviewConfig};

/// `effective`, whole (the `tuning` it also carries is `health`'s, [`crate::store_tuning`]). No `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreEffective {
    pub max_review_rounds: u32,
    pub max_conflict_rounds: u32,
    pub max_check_rounds: u32,
    pub max_slices_per_item: u32,
    pub review: ReviewConfig,
    pub code_gates: CodeGates,
    /// Minutes a Cursor session may run before it is stopped.
    pub cursor_max_minutes: u32,
    /// Helpers an Expert session may start at once.
    pub expert_helpers: u32,
}

impl StoreEffective {
    /// The `effective` of a `settings_get` answer, or `None` when it is missing or not all there.
    pub fn read(value: &Value) -> Option<Self> {
        serde_json::from_value(value.get("effective")?.clone()).ok()
    }

    /// What a project holds before its first `settings_get`: nothing (the tests' stand for a store that has answered).
    #[cfg(not(test))]
    pub fn before_the_first_answer() -> Option<Self> {
        None
    }

    #[cfg(test)]
    pub fn before_the_first_answer() -> Option<Self> {
        Self::read(&serde_json::json!({ "effective": Self::test_json() }))
    }

    /// Lays these over `project`: they are the only source of every value in them.
    pub fn lay(&self, project: &mut ProjectConfig) {
        project.max_review_rounds = self.max_review_rounds;
        project.max_conflict_rounds = self.max_conflict_rounds;
        project.max_check_rounds = self.max_check_rounds;
        project.max_slices_per_item = self.max_slices_per_item;
        project.review = self.review;
        project.code_gates = self.code_gates;
        project.budget.cursor_max_minutes = self.cursor_max_minutes;
        project.expert_helpers = self.expert_helpers;
    }
}

#[cfg(test)]
impl StoreEffective {
    /// The store's defaults, for the tests' fake stores.
    pub fn test_json() -> Value {
        serde_json::json!({
            "maxReviewRounds": 3, "maxConflictRounds": 5, "maxCheckRounds": 3, "maxSlicesPerItem": 10,
            "review": { "maxScoutDiffLines": 4000, "minScoutDiffLines": 2000 },
            "codeGates": { "maxFileLines": 1000, "dupLines": 8, "maxSpecChars": 40000 },
            "cursorMaxMinutes": 60, "expertHelpers": 2,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer() -> Value {
        json!({ "version": 1, "values": {}, "effective": StoreEffective::test_json() })
    }

    #[test]
    fn the_whole_set_reads_and_a_partial_or_missing_one_is_unknown() {
        let e = StoreEffective::read(&answer()).unwrap();
        assert_eq!(
            (e.max_review_rounds, e.code_gates.max_file_lines, e.expert_helpers),
            (3, 1000, 2)
        );
        let mut partial = answer();
        partial["effective"]["codeGates"]
            .as_object_mut()
            .unwrap()
            .remove("dupLines");
        assert_eq!(StoreEffective::read(&partial), None, "one missing number is no set");
        assert_eq!(StoreEffective::read(&json!({ "version": 1, "values": {} })), None);
    }

    #[test]
    fn laid_over_a_project_they_are_the_only_source() {
        let mut p = ProjectConfig::default();
        let mut e = StoreEffective::read(&answer()).unwrap();
        e.max_check_rounds = 9;
        e.code_gates.max_spec_chars = 123;
        e.cursor_max_minutes = 7;
        e.expert_helpers = 5;
        e.lay(&mut p);
        assert_eq!((p.max_check_rounds, p.code_gates.max_spec_chars), (9, 123));
        assert_eq!((p.budget.cursor_max_minutes, p.expert_helpers), (7, 5));
    }
}
