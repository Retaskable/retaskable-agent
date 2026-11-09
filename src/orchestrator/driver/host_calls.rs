//! The `host_call_undefined` events: calls refused by [`crate::host_call::guard`] are written to the log of the
//! project whose repo holds the call's directory, or of every project when there is none (agent-telemetry.md
//! section 3.3). Behind `host-call-undefined-event`: Off, nothing is refused or written.
use std::path::Path;

use serde_json::json;

use super::Machine;
use crate::host_call::{self, Refusal, Sink};
use crate::telemetry::{Event, event};

fn holds(repo_path: &str, dir: &Path) -> bool {
    !repo_path.is_empty() && dir.starts_with(repo_path)
}

impl Machine {
    /// Sets the guard from the flag (On for any project) and writes what it refused since the last pass.
    pub(super) async fn write_host_calls(&mut self) {
        let mut on = false;
        let asked: Vec<_> = self.projects.iter().map(|lp| lp.flags.clone()).collect();
        for flags in asked {
            on |= flags.on(host_call::FLAG).await;
        }
        host_call::set_on(on);
        if !on {
            return;
        }
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
