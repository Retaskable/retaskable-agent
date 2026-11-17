//! A send-back from a state that holds no lane (CI's red in `awaiting_ci`, a conflict or a person's try-out in `merging` or
//! `awaiting_playtest`: `needsLane` in `steps.ts` is only the implementing, checking and reviewing states). The item is
//! `implementing` again but has no lane, so it waits for one here, like a Continue does; the session that starts on it is a new one
//! on the item's pushed branch, with the picking-up prompt around what it was sent back with.
use super::project::Launch;
use super::{ProjectLoop, SessionStarter, Shared};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::LanePool;
use crate::orchestrator::fresh_start::picking_up_prompt;

impl ProjectLoop {
    /// Queues `item` to start its implementer again on a lane, with the record's follow-up.
    pub(super) fn wait_for_lane(&mut self, item: &str) {
        if !self.lane_resumes.iter().any(|i| i == item) {
            self.lane_resumes.push(item.to_string());
        }
    }

    /// Starts the items waiting for a lane that can: one stays queued while no session slot, no lane or no start is free.
    pub(super) async fn start_lane_resumes(
        &mut self,
        pool: &mut LanePool,
        room: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) {
        if shared.stopping {
            return;
        }
        for item in std::mem::take(&mut self.lane_resumes) {
            let waiting = self.runs.get(&item).is_some_and(|r| r.state == ItemState::Implementing) && self.holds(&item);
            if !waiting {
                continue;
            }
            if *room == 0 {
                self.lane_resumes.push(item);
                continue;
            }
            let Some(lane) = self.take_lane(pool, &item) else {
                self.lane_resumes.push(item);
                continue;
            };
            if self.start_held(pool, shared, lane) {
                self.give_back_lane(pool, &item, lane);
                self.lane_resumes.push(item);
                continue;
            }
            let has_pr = self.records.get(&item).is_some_and(|r| r.flow.pr_url.is_some());
            let message = (self.records.get_mut(&item))
                .and_then(|r| r.flow.follow_up.take())
                .unwrap_or_else(|| "Carry on with the slice.".into());
            let prompt = picking_up_prompt(has_pr, &self.project.base_branch, &message);
            tracing::info!(
                "{}: starting {item} again on lane {lane}, sent back from a state with no lane",
                self.id
            );
            *room -= u32::from(
                self.launch(pool, &item, lane, Launch::Continuing(prompt), starter, shared)
                    .await,
            );
        }
    }
}
