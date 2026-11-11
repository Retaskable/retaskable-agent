//! The `host_call_undefined` events: calls refused by [`crate::host_call::guard`] are written to the log of the
//! project whose repo holds the call's directory, or of every project when there is none (agent-telemetry.md
//! section 3.3).
use std::path::Path;

use serde_json::json;

use super::Machine;
use crate::host_call::{Refusal, Sink};
use crate::telemetry::{Event, event};

fn holds(repo_path: &str, dir: &Path) -> bool {
    !repo_path.is_empty() && dir.starts_with(repo_path)
}

impl Machine {
    /// Writes what the guard refused since the last pass to the logs of the projects it was for. The sink is
    /// drained every pass, so nothing stale is written later.
    pub(super) fn write_host_calls(&mut self) {
        for Refusal { cmd, args, dir } in Sink::global().take() {
            let mine: Vec<usize> = (0..self.projects.len())
                .filter(|&i| {
                    dir.as_deref()
                        .is_some_and(|d| holds(&self.projects[i].project.repo_path, d))
                })
                .collect();
            let to = if mine.is_empty() {
                (0..self.projects.len()).collect()
            } else {
                mine
            };
            for i in to {
                let lp = &mut self.projects[i];
                let id = lp.id.clone();
                lp.events.add_in(
                    &id,
                    Event::HostCallUndefined(event::HostCallUndefined {
                        cmd: Some(cmd.clone()),
                        args: Some(json!(args)),
                        ..Default::default()
                    }),
                );
            }
        }
    }
}
