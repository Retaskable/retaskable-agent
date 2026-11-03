//! Item commands the loop carries out on its pass (agent/09-web-ui.md section 9.3). The connector's thread
//! only queues them ([`crate::view::ItemCommands`]); the state moves here, in plain code.
//!
//! **Continue** on an item that is `needs_human` in `runs`: an item in any other state, or unknown here, has
//! its command dropped and logged. With nothing on a branch to carry on from (`triage::schedule::nothing_to_continue`)
//! the item starts over, as **Start over** does. Otherwise it waits for a free lane, then `ItemRun::resume_from_needs_human`
//! (or `resume_past_slice_limit` at the slice limit) clears its rounds and the implementer starts on the existing
//! branch with the notes as its prompt (`Launch::Continuing`, the lane reused as it is). With no notes it goes
//! back to `checking`, so its checks run again.
//!
//! **Start over** (`retry`) on a `needs_human` item (agent/09-web-ui.md section 9.3) waits for a free lane, then
//! lets go of the old work ([`SessionStarter::abandon`]: `gh pr close`, or the branch dropped when there is no PR; a
//! merged slice is never touched), bumps the item's attempt, clears its rounds (`resume_from_needs_human`) and starts
//! a fresh implementer on the new branch (`…-s<slice>-r<attempt>`, prepared from `origin/<base>`), the notes in its
//! first prompt. If the old work can't be let go of, the item stays as it is.
//!
//! Claude's own decisions (`triage_run.rs`) are queued here too, marked in `claude_pending`: a person's Continue, Start
//! over or Skip for the item drops Claude's, and one waiting for a lane stays `claudeOnIt`.
//!
//! **Skip** (`skip`, and a triage `skip` decision, `claude`) on a `needs_human` item ([`ProjectLoop::skip_stopped`]) drops its
//! run, frees its lane, gives the lease back with a note (`report`, `release`) and then skips the item in the store
//! (`skip`), so no computer claims it again. Its branch and pull request are kept.
use super::project::signed_out;
use super::{Abandon, Launch, ProjectLoop, SessionStarter, Shared};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::{Holder, LanePool, Want};
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
        for command in shared.links.item_commands.take(&self.id) {
            if let ItemCommand::Continue { item, .. }
            | ItemCommand::StartOver { item, .. }
            | ItemCommand::Skip { item, .. } = &command
            {
                self.drop_claudes(item);
            }
            let (name, item, notes, start_over) = match command {
                ItemCommand::Continue { item, notes, .. } => ("continue", item, notes, false),
                ItemCommand::StartOver { item, notes, .. } => ("start over", item, notes, true),
                ItemCommand::Skip { item, .. } => {
                    self.skip_stopped(pool, &item, false, shared).await;
                    continue;
                }
                ItemCommand::Play { .. } | ItemCommand::Stop { .. } => continue,
            };
            let stopped = self.runs.get(&item).is_some_and(|r| r.state == ItemState::NeedsHuman);
            let waiting = self
                .continuing
                .iter()
                .chain(&self.starting_over)
                .any(|(id, _)| *id == item);
            if !stopped || waiting {
                tracing::info!("{}: dropped a {name} for {item}: it isn't stopped here", self.id);
            } else {
                let queue = if start_over {
                    &mut self.starting_over
                } else {
                    &mut self.continuing
                };
                queue.push((item, notes.filter(|n| !n.trim().is_empty())));
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
        for (item, notes) in std::mem::take(&mut self.starting_over) {
            if !self
                .start_over(&item, notes.as_deref(), (pool, starter), (&mut room, shared))
                .await
            {
                self.starting_over.push((item, notes));
            }
        }
        let waiting: Vec<&String> = self
            .continuing
            .iter()
            .chain(&self.starting_over)
            .map(|(id, _)| id)
            .collect();
        self.claude_pending.retain(|id| waiting.contains(&id));
    }

    /// A person acted on `item_id`, so Claude's decision waiting for a lane is dropped (06.9 bullet 17).
    fn drop_claudes(&mut self, item_id: &str) {
        if self.claude_pending.remove(item_id) {
            self.continuing.retain(|(id, _)| id != item_id);
            self.starting_over.retain(|(id, _)| id != item_id);
        }
    }

    /// "Claude" or "a person": who chose what is being carried out for `item_id`.
    fn chooser(&self, item_id: &str) -> &'static str {
        if self.claude_pending.contains(item_id) {
            "Claude"
        } else {
            "a person"
        }
    }

    /// Skips a stopped item (`claude`: the triager's decision, else a person's button): the run is dropped (`item_state`, `needs_human` to `skipped`), the lane freed and the
    /// lease released with a note, and the skip is kept owed (`unskipped`) until the store has taken the release and
    /// the `skip`; `flush_owed` sends both again on later passes. An item that isn't stopped here has its command dropped and logged.
    pub(super) async fn skip_stopped(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        claude: bool,
        shared: &mut Shared<'_>,
    ) {
        let (who, lower) = if claude {
            ("Claude", "Claude")
        } else {
            ("A person", "a person")
        };
        if !self.runs.get(item_id).is_some_and(|r| r.state == ItemState::NeedsHuman) {
            tracing::info!("{}: dropped a skip for {item_id}: it isn't stopped here", self.id);
            return;
        }
        self.runs.remove(item_id);
        self.items.remove(item_id);
        self.free_lane(pool, item_id, "skipped", &format!("{lower} chose Skip"));
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item_id.to_string()),
                from: Some("needs_human".into()),
                to: Some("skipped".into()),
                note: Some(format!("{lower} chose Skip")),
                ..Default::default()
            }),
        );
        let Some(lease) = self.leases.remove(item_id) else {
            return;
        };
        self.unreleased
            .insert(item_id.to_string(), (lease, format!("{who} chose Skip.")));
        self.unskipped.insert(item_id.to_string());
        self.save_leases();
        self.flush_owed(shared.links).await;
    }

    fn want(&self, item_id: &str) -> Want {
        Want {
            project: self.id.clone(),
            item: item_id.to_string(),
            chore: false,
            last_lane: self.last_lane.get(item_id).copied(),
            fix_queued: false,
        }
    }

    fn lane_taken(&mut self, item_id: &str, lane: u32) {
        self.events.add_in(
            &self.id,
            Event::LaneTaken(event::LaneTaken {
                item: Some(item_id.to_string()),
                lane: Some(lane),
                ..Default::default()
            }),
        );
    }

    /// Claims a lane for the item and records it.
    fn take_lane(&mut self, pool: &mut LanePool, item_id: &str) -> Option<u32> {
        let lane = pool.claim(&self.want(item_id))?;
        self.lane_taken(item_id, lane);
        Some(lane)
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
            tracing::info!("{}: {item_id} has nothing to continue from; starting it over", self.id);
            return self
                .start_over(item_id, notes.as_deref(), (pool, starter), (room, shared))
                .await;
        }
        let checks_only = notes.is_none() && !at_limit;
        if !checks_only && *room == 0 {
            return false;
        }
        let Some(lane) = self.take_lane(pool, item_id) else {
            return false;
        };
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
        let who = self.chooser(item_id);
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item_id.to_string()),
                from: Some("needs_human".into()),
                to: Some(state_name(to).into()),
                lane: Some(lane),
                note: Some(format!("{who} chose Continue")),
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

    /// Carries out one Start over. False when it must wait for a lane or a session slot.
    async fn start_over(
        &mut self,
        item_id: &str,
        notes: Option<&str>,
        (pool, starter): (&mut LanePool, &dyn SessionStarter),
        (room, shared): (&mut u32, &mut Shared<'_>),
    ) -> bool {
        if !self.runs.get(item_id).is_some_and(|r| r.state == ItemState::NeedsHuman) {
            tracing::info!("{}: dropped a start over for {item_id}: it isn't stopped here", self.id);
            return true;
        }
        if *room == 0 {
            return false;
        }
        let Some(lane) = pool.claim(&self.want(item_id)) else {
            return false;
        };
        let item = self.items.get(item_id);
        let old = Abandon {
            project: self.id.clone(),
            branch: self
                .records
                .get(item_id)
                .and_then(|r| r.branch.clone())
                .or(item.and_then(|i| i.branch.clone())),
            pr_url: item.and_then(|i| i.pr_url.clone()),
        };
        if let Err(why) = starter.abandon(old).await {
            tracing::warn!("{}: didn't start {item_id} over: {why}", self.id);
            let holder = Holder::Item {
                project: self.id.clone(),
                item: item_id.to_string(),
            };
            pool.release(lane, &holder);
            return true;
        }
        self.lane_taken(item_id, lane);
        let who = self.chooser(item_id);
        let attempt = self.attempt_of(item_id) + 1;
        let slice = self.runs.get(item_id).map_or(0, |r| r.slice);
        if let Some(record) = self.records.get_mut(item_id) {
            (record.attempt, record.slice) = (attempt, slice);
        }
        let run = self.runs.get_mut(item_id).expect("checked above");
        run.resume_from_needs_human();
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item_id.to_string()),
                from: Some("needs_human".into()),
                to: Some("implementing".into()),
                lane: Some(lane),
                note: Some(format!("{who} chose Start over")),
                ..Default::default()
            }),
        );
        self.remember(item_id, Some(lane), None, None);
        *room -= u32::from(
            self.launch(
                pool,
                item_id,
                lane,
                Launch::StartOver(notes.map(str::to_string)),
                starter,
                shared,
            )
            .await,
        );
        true
    }
}
