//! Unlink this project (`unlink_project { removeLanes? }`; `unlinkProject.ts` and `runningWork` in the TS agent, projects.md
//! section 6.6): retaskable.com's request to take the project off this computer. Nothing on GitHub changes. The command's
//! thread queues it ([`crate::view::ProjectCommand::Unlink`]) and the loop, which alone knows what runs, takes it on the next
//! pass of a loop that acts (`claim` or `runs` mode, from `wanted_passes`):
//!
//! 1. **Refused while anything runs in the project** (a session, a step, Play, Play on phone), in TS's words: "<what> in
//!    <repo>: pause it and let it finish first." The command is nacked and nothing changes.
//! 2. Otherwise the command is answered **first** (`ack`, so the website hears it before the hub goes), and the project takes
//!    no new work from here on (its mode goes `off`).
//! 3. It gives back what this computer holds in the shared backlog (the store's `leave`: items, waits, chore locks, drafts), so
//!    teammates need not wait out a lease. A store that doesn't answer in 10 s, or can't be reached, is logged and the unlink
//!    goes on: the leases run out on their own.
//! 4. The project is taken out of `config.json` and its start-paused marker is forgotten, and, unless `removeLanes` is false,
//!    its lanes and checkouts are removed (a lane that won't go is logged). A config that can't be written puts the project back
//!    as it was, and the command's thread is told why.
//!
//! The command's own thread then removes the state folder and restarts the agent, which builds every part from the project
//! list at start (the TS agent closes the project live).
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::{Links, ProjectLoop};
use crate::lane_pool::LanePool;
use crate::view::ProjectCommand;

/// How long the store has to answer `leave`.
const LEAVE_LIMIT: Duration = Duration::from_secs(10);

/// What the store says it took back.
#[derive(Debug, Deserialize)]
struct Gave {
    items: u32,
    locks: u32,
    drafts: u32,
}

impl ProjectLoop {
    /// Why the project can't be unlinked now, or `None` when nothing runs in it (`runningWork`).
    pub(super) fn running_work(&mut self, pool: &LanePool) -> Option<&'static str> {
        let implementing = (self.runs.iter())
            .any(|(id, r)| r.state == crate::item_lifecycle::ItemState::Implementing && !self.chores.contains_key(id));
        if implementing
            || !self.to_resume.is_empty()
            || self.asking() > 0
            || self.drafting()
            || self.experting() > 0
            || self.jobs.session_running()
            || self.run_loop.runs().sessions() > 0
        {
            return Some("Sessions are running");
        }
        if self.runs.keys().any(|id| self.running.busy(id)) {
            return Some("A step is running (checks or a merge)");
        }
        let playing = pool
            .held_lanes()
            .filter_map(|k| pool.holder(k))
            .any(|h| matches!(h, crate::lane_pool::Holder::Play { project, .. } if *project == self.id));
        if playing || crate::play::running(&self.id) {
            return Some("Play is running");
        }
        if crate::phone_run::busy(&self.id) {
            return Some("Play on phone is running");
        }
        None
    }

    /// Carries out the commands on the project as a whole queued for it: the unlinks (see the module's header) and the release
    /// note's page (`release_page.rs`).
    pub(super) async fn project_pass(&mut self, pool: &LanePool, links: &Links) {
        for command in links.item_commands.projects.take(&self.id) {
            let (command_id, remove_lanes, done) = match command {
                ProjectCommand::Unlink {
                    command_id,
                    remove_lanes,
                    done,
                    ..
                } => (command_id, remove_lanes, done),
                ProjectCommand::Release {
                    command_id,
                    unpublish,
                    link,
                    ..
                } => {
                    self.release_page(command_id, unpublish, link, links).await;
                    continue;
                }
            };
            let project_id = self.id.clone();
            let answer = |msg: serde_json::Value| links.item_commands.answer(&project_id, msg);
            if let Some(why) = self.running_work(pool) {
                let repo = if self.project.github_repo.is_empty() {
                    self.id.clone()
                } else {
                    self.project.github_repo.clone()
                };
                let reason = format!("{why} in {repo}: pause it and let it finish first.");
                answer(json!({ "type": "nack", "id": command_id, "reason": reason }));
                done.send(Err(reason));
                continue;
            }
            let Some(dir) = self.data_dir.clone() else {
                let reason = "This computer's settings can't be found.".to_string();
                answer(json!({ "type": "nack", "id": command_id, "reason": reason }));
                done.send(Err(reason));
                continue;
            };
            answer(json!({ "type": "ack", "id": command_id }));
            let before = std::mem::replace(&mut self.mode, super::OrchestratorMode::Off);
            self.give_back_to_store(links).await;
            let (id, project) = (self.id.clone(), self.project.clone());
            let removed = tokio::task::spawn_blocking(move || {
                crate::unlink::remove_from_config(&dir, &id)?;
                if let Err(e) = crate::start_paused::forget(&dir, &id) {
                    tracing::warn!("Couldn't forget {id}'s start-paused marker: {e:#}");
                }
                if remove_lanes
                    && let Err(e) = crate::unlink::remove_lanes(
                        std::path::Path::new(&project.repo_path),
                        std::path::Path::new(&project.worktrees_dir),
                    )
                {
                    tracing::error!("Couldn't remove every lane of {id}: {e:#}");
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("the unlink task crashed: {e}")));
            match removed {
                Ok(()) => {
                    tracing::info!("{}: unlinked; the agent restarts without it", self.id);
                    done.send(Ok(()));
                }
                Err(e) => {
                    tracing::error!("Couldn't unlink {}: {e:#}", self.id);
                    self.mode = before;
                    done.send(Err(format!("{e:#}")));
                }
            }
        }
    }

    /// Gives back what this computer holds in the shared backlog (the store's `leave`). Logged, never fatal.
    async fn give_back_to_store(&mut self, links: &Links) {
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            tracing::error!(
                "Couldn't give {}'s items back to the shared backlog (no link to it), so they run out on their own.",
                self.id
            );
            return;
        };
        let why = format!("unlinked on {}", links.machine);
        let call = self
            .client
            .op::<Gave>(repo, Some(&token), "leave", json!({ "why": why }), "items it gave back");
        match tokio::time::timeout(LEAVE_LIMIT, call).await {
            Ok(Ok(gave)) => tracing::info!(
                "Gave {}'s {} item(s), {} draft(s) and {} lock(s) back to the shared backlog.",
                self.id,
                gave.items,
                gave.drafts,
                gave.locks
            ),
            Ok(Err(e)) => tracing::error!(
                "Couldn't give {}'s items back to the shared backlog, so they run out on their own: {e}",
                self.id
            ),
            Err(_) => tracing::error!(
                "Couldn't give {}'s items back to the shared backlog, so they run out on their own: the store didn't answer in time",
                self.id
            ),
        }
    }
}
