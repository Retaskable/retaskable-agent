//! Item commands the loop carries out on its pass (agent/09-web-ui.md section 9.3). The connector's thread
//! only queues them ([`crate::view::ItemCommands`]); the state moves here, in plain code.
//!
//! **Continue** on an item that is `needs_human` in `runs`: an item in any other state, or unknown here, has
//! its command dropped and logged. With nothing on a branch to carry on from (`triage::schedule::nothing_to_continue`)
//! the item is left for Start over. Otherwise it waits for a free lane, then `ItemRun::resume_from_needs_human`
//! (or `resume_past_slice_limit` at the slice limit) clears its rounds and the implementer starts on the existing
//! branch with the notes as its prompt (`Launch::Continuing`, the lane reused as it is). With no notes it goes
//! back to `checking`, so its checks run again.
use super::project::signed_out;
use super::{Launch, ProjectLoop, SessionStarter, Shared};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::{LanePool, Want};
use crate::orchestrator::steps::state_name;
use crate::telemetry::{Event, event};
use crate::triage::schedule::nothing_to_continue;
use crate::view::ItemCommand;

/// A Continue waiting for a lane: the item and the notes (picture paths included) to send.
pub(super) type PendingContinue = (String, Option<String>);

impl ProjectLoop {
    /// Takes in this project's queued commands, then carries on every Continue that can start. `room` is how
    /// many more sessions may run on the computer.
    pub(super) async fn carry_out_commands(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        mut room: u32,
        shared: &mut Shared<'_>,
    ) {
        self.release_finished_games(pool);
        for command in shared.links.item_commands.take(&self.id) {
            let (item, notes) = match command {
                ItemCommand::Continue { item, notes, .. } => (item, notes),
                ItemCommand::Play { lane, .. } => {
                    if !shared.stopping {
                        self.play_lane(pool, lane, starter);
                    }
                    continue;
                }
                ItemCommand::Stop { lane, .. } => {
                    self.stop_lane(pool, lane);
                    continue;
                }
            };
            let stopped = self.runs.get(&item).is_some_and(|r| r.state == ItemState::NeedsHuman);
            if !stopped || self.continuing.iter().any(|(id, _)| *id == item) {
                tracing::info!("{}: dropped a continue for {item}: it isn't stopped here", self.id);
            } else {
                self.continuing.push((item, notes.filter(|n| !n.trim().is_empty())));
            }
        }
        if signed_out(shared) || shared.stopping {
            return;
        }
        for (item, notes) in std::mem::take(&mut self.continuing) {
            if !self
                .continue_item(pool, &item, &notes, &mut room, starter, shared)
                .await
            {
                self.continuing.push((item, notes));
            }
        }
    }

    /// Carries out one Continue. False when it must wait for a lane or a session slot.
    async fn continue_item(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        notes: &Option<String>,
        room: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) -> bool {
        let Some(run) = self.runs.get(item_id).filter(|r| r.state == ItemState::NeedsHuman) else {
            tracing::info!("{}: dropped a continue for {item_id}: it isn't stopped here", self.id);
            return true;
        };
        let at_limit = run.slice >= self.project.max_slices_per_item + run.extra_slices;
        let record = self.records.get(item_id);
        let branch =
            (record.and_then(|r| r.branch.as_deref())).or(self.items.get(item_id).and_then(|i| i.branch.as_deref()));
        let pr_url = self.items.get(item_id).and_then(|i| i.pr_url.as_deref());
        let session = record.and_then(|r| r.impl_session_id.as_deref());
        if !at_limit && nothing_to_continue(branch, pr_url, session) {
            tracing::info!(
                "{}: {item_id} has nothing to continue from; it waits for Start over",
                self.id
            );
            return true;
        }
        let checks_only = notes.is_none() && !at_limit;
        if !checks_only && *room == 0 {
            return false;
        }
        let want = Want {
            project: self.id.clone(),
            item: item_id.to_string(),
            chore: false,
            last_lane: self.last_lane.get(item_id).copied(),
            fix_queued: false,
        };
        let Some(lane) = pool.claim(&want) else { return false };
        self.events.add_in(
            &self.id,
            Event::LaneTaken(event::LaneTaken {
                item: Some(item_id.to_string()),
                lane: Some(lane),
                ..Default::default()
            }),
        );
        let run = self.runs.get_mut(item_id).expect("checked above");
        if at_limit {
            run.resume_past_slice_limit(&self.project);
        } else {
            run.resume_from_needs_human();
            if checks_only {
                run.state = ItemState::Checking;
            }
        }
        let to = run.state;
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item_id.to_string()),
                from: Some("needs_human".into()),
                to: Some(state_name(to).into()),
                lane: Some(lane),
                note: Some("a person chose Continue".into()),
                ..Default::default()
            }),
        );
        if to == ItemState::Implementing {
            let kind = match notes {
                Some(text) => Launch::Continuing(text.clone()),
                None => Launch::Fresh,
            };
            *room -= u32::from(self.launch(pool, item_id, lane, kind, starter, shared).await);
        } else {
            self.remember(item_id, Some(lane), None, None);
            self.save_leases();
        }
        true
    }
}
