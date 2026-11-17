//! What a project takes from the store beyond its tasks: the numbers of `health`'s `tuning` (pools-on-server.md section 14.6)
//! and the settings it holds a default for, from `settings_get`'s `effective` (section 18.5: round caps, scout lines, code gates,
//! `cursorMaxMinutes`, Expert helpers).
//! The agent holds none of its own: until the store has sent its numbers nothing that needs one starts (fail closed).
use super::project::ProjectLoop;
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
        self.project.tuning.store.is_some() && self.effective.is_some()
    }

    /// Lays the store's `effective` settings over `project`: the round caps, the scout lines, the code gates, the Cursor minutes and
    /// the Expert helpers have no other source (`file_project` carries none of them).
    pub(super) fn lay_effective(&mut self) {
        if let Some(e) = &self.effective {
            e.lay(&mut self.project);
        }
    }
}
