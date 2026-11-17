//! What a project takes from the store beyond its tasks: the numbers of `health`'s `tuning` (pools-on-server.md section 14.6)
//! and the round caps of its settings (`maxReviewRounds`, `maxCheckRounds`, `maxConflictRounds`, the site's project settings).
//! The agent holds none of its own: until the store has sent its numbers nothing that needs one starts (fail closed).
use serde_json::Value;

use super::project::ProjectLoop;
use crate::config::ProjectConfig;
use crate::store_tuning::StoreTuning;

impl ProjectLoop {
    /// The numbers of the store's last `health` answer (`None`: it carried none): laid over the project the flows read, and
    /// over `file_project` so the next pass's allowed commands keep them.
    pub(super) fn take_store_tuning(&mut self, tuning: Option<StoreTuning>) {
        self.project.tuning.store = tuning;
        self.file_project.tuning.store = tuning;
    }

    /// Whether the store's numbers are known: what is started, resumed or stepped needs them.
    pub(super) fn has_store_numbers(&self) -> bool {
        self.project.tuning.store.is_some()
    }

    /// Lays the project's saved round caps over `project` (`file_project` has the file's own): the site's settings win.
    pub(super) fn lay_round_caps(&mut self) {
        round_caps(&mut self.project, &self.settings_values);
    }
}

/// The round caps in `values` (a whole number from 0 to 100, as the store checks them) over `project`'s.
fn round_caps(project: &mut ProjectConfig, values: &Value) {
    let cap = |key: &str| {
        values
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n <= 100)
            .and_then(|n| u32::try_from(n).ok())
    };
    if let Some(n) = cap("maxReviewRounds") {
        project.max_review_rounds = n;
    }
    if let Some(n) = cap("maxCheckRounds") {
        project.max_check_rounds = n;
    }
    if let Some(n) = cap("maxConflictRounds") {
        project.max_conflict_rounds = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_sites_round_caps_win_over_the_file_and_a_bad_value_is_ignored() {
        let mut project = ProjectConfig::default();
        let file = (
            project.max_review_rounds,
            project.max_check_rounds,
            project.max_conflict_rounds,
        );
        round_caps(&mut project, &Value::Null);
        assert_eq!(
            file,
            (
                project.max_review_rounds,
                project.max_check_rounds,
                project.max_conflict_rounds
            )
        );
        round_caps(
            &mut project,
            &json!({ "maxReviewRounds": 7, "maxCheckRounds": 0, "maxConflictRounds": 101 }),
        );
        assert_eq!(project.max_review_rounds, 7);
        assert_eq!(project.max_check_rounds, 0, "0 is a cap too");
        assert_eq!(
            project.max_conflict_rounds, file.2,
            "out of the store's range: left alone"
        );
        round_caps(
            &mut project,
            &json!({ "maxReviewRounds": "many", "maxCheckRounds": -1 }),
        );
        assert_eq!((project.max_review_rounds, project.max_check_rounds), (7, 0));
    }
}
