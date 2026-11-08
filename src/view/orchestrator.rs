//! The snapshot's `orchestrator` block (cloud.md section 5.2): what the project's loop is doing.
use serde_json::{Value, json};

use super::{AgentView, NO_WORK_YET};
use crate::telemetry::debounce::OpenAnomaly;
use crate::telemetry::events::{MAX_TEXT, iso_utc_millis, scrub};

/// The most open anomalies one snapshot carries; the rest are counted in `anomaliesMore`.
const MAX_ANOMALIES: usize = 20;
/// The longest an anomaly's `details` is written out.
const MAX_DETAILS: usize = 1024;

/// `details` as is when it writes out within [`MAX_DETAILS`]; otherwise only its first keys that fit
/// (scrubbed like the log's text, since it also reaches the site).
fn cut_details(details: &Value) -> Value {
    let details = crate::telemetry::events::clean(details, 0);
    if details.to_string().len() <= MAX_DETAILS {
        return details;
    }
    let Value::Object(map) = details else {
        return json!({});
    };
    let mut kept = serde_json::Map::new();
    for (k, v) in map {
        kept.insert(k.clone(), v);
        if Value::Object(kept.clone()).to_string().len() > MAX_DETAILS {
            kept.remove(&k);
            break;
        }
    }
    Value::Object(kept)
}

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
        if !project.is_empty() && self.settings.snapshot_reporting_on() {
            let (spent, daily) = self.settings.spend_today(project);
            block["spendTodayUsd"] = json!(spent);
            block["dailyUsd"] = json!(daily);
        }
        if let Some(anomalies) = self.anomalies_block() {
            block["anomalies"] = anomalies.0;
            if anomalies.1 > 0 {
                block["anomaliesMore"] = json!(anomalies.1);
            }
        }
        if let Some(restart) = self.restart.report() {
            block["restart"] = restart;
        }
        block
    }

    /// With `snapshot-reporting` On, every project's open anomalies (oldest first, at most [`MAX_ANOMALIES`]) and
    /// how many more there are; `None` when there are none.
    fn anomalies_block(&self) -> Option<(Value, usize)> {
        if !self.settings.snapshot_reporting_on() {
            return None;
        }
        let state = self.live.snapshot();
        let mut all: Vec<(&str, &OpenAnomaly)> = state
            .projects
            .iter()
            .flat_map(|(id, p)| p.anomalies.iter().map(move |a| (id.as_str(), a)))
            .collect();
        if all.is_empty() {
            return None;
        }
        all.sort_by(|a, b| a.1.since_ms.cmp(&b.1.since_ms).then_with(|| a.1.id.cmp(&b.1.id)));
        let more = all.len().saturating_sub(MAX_ANOMALIES);
        let sent: Vec<Value> = all
            .into_iter()
            .take(MAX_ANOMALIES)
            .map(|(project, a)| {
                let mut out = json!({
                    "id": a.id,
                    "kind": a.rule,
                    "since": iso_utc_millis(a.since_ms),
                    "text": scrub(&a.text, MAX_TEXT),
                    "details": cut_details(&a.details),
                    "project": project,
                });
                if let Some(item) = &a.item {
                    out["item"] = json!(item);
                }
                out
            })
            .collect();
        Some((Value::Array(sent), more))
    }

    /// With `snapshot-reporting` On, the items the project's loop holds (`items`) and, when one is a chore, the Chores
    /// milestone; `None` when the flag is Off.
    pub(crate) fn held_items_block(&self, project: &str) -> Option<(Value, Value)> {
        if !self.settings.snapshot_reporting_on() {
            return None;
        }
        let state = self.live.snapshot();
        let held: Vec<_> = state
            .projects
            .get(project)
            .into_iter()
            .flat_map(|p| p.items.iter())
            .collect();
        let items = held
            .iter()
            .map(|i| {
                let mut out =
                    json!({ "id": i.id, "title": i.title, "state": i.state, "attempt": i.attempt, "chore": i.chore });
                if let Some(lane) = i.lane {
                    out["lane"] = json!(lane);
                }
                if let Some(branch) = &i.branch {
                    out["branch"] = json!(branch);
                }
                if let Some(pr) = &i.pr_url {
                    out["prUrl"] = json!(pr);
                }
                out
            })
            .collect();
        let milestones = if held.iter().any(|i| i.chore) {
            json!([{ "id": "chores", "title": "Chores" }])
        } else {
            json!([])
        };
        Some((milestones, Value::Array(items)))
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
