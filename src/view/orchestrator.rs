//! The snapshot's `orchestrator` block (cloud.md section 5.2): what the project's loop is doing.
use serde_json::{Value, json};

use super::{AgentView, NO_WORK_YET};

impl AgentView {
    /// The `orchestrator` block for `project` ("" before Setup). With `snapshot-reporting` Off, or for a project
    /// whose loop isn't running, the project reads as paused with [`NO_WORK_YET`] unless a person's pause set a reason.
    pub(super) fn orchestrator_block(&self, project: &str) -> Value {
        let mut block = if project.is_empty() {
            json!({ "status": "setup", "machine": self.machine })
        } else {
            match self.loop_state(project) {
                Some(live) => self.live_block(project, &live),
                None => {
                    let reason = self.paused.reason(project).unwrap_or_else(|| NO_WORK_YET.to_string());
                    json!({ "status": "paused", "machine": self.machine, "pausedReason": reason })
                }
            }
        };
        if let Some(restart) = self.restart.report() {
            block["restart"] = restart;
        }
        block
    }

    /// The project's loop as of its last pass, when the flag is On and the loop runs.
    fn loop_state(&self, project: &str) -> Option<crate::orchestrator::live::LiveState> {
        if !self.settings.snapshot_reporting_on() {
            return None;
        }
        let state = self.live.snapshot();
        state.projects.contains_key(project).then_some(state)
    }

    fn live_block(&self, project: &str, state: &crate::orchestrator::live::LiveState) -> Value {
        let mut block = json!({ "machine": self.machine, "concurrency": state.concurrency });
        if let Some(reason) = self.paused.reason(project) {
            block["status"] = json!("paused");
            block["pausedReason"] = json!(reason);
        } else if let Some(at) = &state.projects[project].limit_resets_at {
            block["status"] = json!("waiting_for_limit");
            block["resetsAt"] = json!(at);
        } else {
            block["status"] = json!("running");
        }
        if let Some((allowed, _)) = self.item_commands.auto() {
            block["concurrency"] = json!(allowed);
        }
        block
    }
}
