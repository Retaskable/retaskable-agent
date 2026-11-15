//! The passes that aren't about an item, run in `claim` mode (from `schedule`) and in `runs` mode (from
//! `run_passes`): the chat's asks, expert sessions, a person's Build this, drafts and the trial plan. What decides where they run is [`Machine::acts`], and
//! what they count against `concurrency` is [`Machine::sessions`].
use super::{Machine, Shared};

impl Machine {
    /// Sessions running now that no pass counts itself (asks, drafts, experts and the trial count theirs): the
    /// claim starter's, and the live runs that hold a session. A watch run polls and a merge run only calls `gh`,
    /// so neither takes a slot.
    pub(super) fn sessions(&self) -> u32 {
        let claimed = self.starter.as_ref().map_or(0, |s| s.active());
        let runs: u32 = (self.projects.iter()).map(|lp| lp.run_loop.runs().sessions()).sum();
        claimed + runs
    }

    /// Answers the chat's asks for the `on` projects. Asks have a person waiting: they take their slots first, never
    /// wait for one, and count toward `concurrency`. A pause holds new ones back, not the ones running.
    pub(super) async fn ask_passes(&mut self, on: &[bool], paused: &[bool], stopping: bool, slots: &mut u32) {
        let Some(host) = self.asks.clone() else { return };
        *slots = slots.saturating_sub(self.projects.iter_mut().map(|lp| lp.asking()).sum());
        for ((lp, &on), &paused) in self.projects.iter_mut().zip(on).zip(paused) {
            if on {
                let shared = Shared {
                    links: &self.links,
                    local: &self.local,
                    claim_paused_until: &mut self.claim_paused_until,
                    claude_signed_out_until: &mut self.claude_signed_out_until,
                    engine_limits: &mut self.engine_limits,
                    stopping,
                };
                lp.ask_pass(paused, &self.pool, slots, &host, &shared).await;
            }
        }
    }

    /// Writes up a person's Build this as a draft in the store, for the `on` projects.
    pub(super) async fn idea_passes(&mut self, on: &[bool], stopping: bool) {
        for (lp, &on) in self.projects.iter_mut().zip(on) {
            if on {
                let shared = Shared {
                    links: &self.links,
                    local: &self.local,
                    claim_paused_until: &mut self.claim_paused_until,
                    claude_signed_out_until: &mut self.claude_signed_out_until,
                    engine_limits: &mut self.engine_limits,
                    stopping,
                };
                lp.idea_pass(&shared).await;
            }
        }
    }

    /// Runs the expert sessions of the `on` projects. A person is in them: their turns take slots too, and a pause
    /// doesn't hold them back.
    pub(super) async fn expert_passes(&mut self, on: &[bool], stopping: bool, slots: &mut u32) {
        let Some(host) = self.experts.clone() else { return };
        *slots = slots.saturating_sub(self.projects.iter().map(|lp| lp.experting()).sum());
        for (lp, &on) in self.projects.iter_mut().zip(on) {
            if on {
                let shared = Shared {
                    links: &self.links,
                    local: &self.local,
                    claim_paused_until: &mut self.claim_paused_until,
                    claude_signed_out_until: &mut self.claude_signed_out_until,
                    engine_limits: &mut self.engine_limits,
                    stopping,
                };
                lp.expert_pass(&mut self.pool, &host, &shared).await;
            }
        }
    }

    /// Claims and lands the spec drafts of the `on` projects. Drafts jump the queue, so they take their slots before
    /// any item is planned, and count toward `concurrency`. A pause holds new drafts back, not landing one.
    pub(super) async fn draft_passes(&mut self, on: &[bool], paused: &[bool], stopping: bool, slots: &mut u32) {
        let Some(host) = self.drafts.clone() else { return };
        *slots = slots.saturating_sub(
            self.projects
                .iter_mut()
                .filter_map(|lp| lp.drafting().then_some(()))
                .count() as u32,
        );
        let lanes = self.pool.rules().lanes;
        for ((lp, &on), &paused) in self.projects.iter_mut().zip(on).zip(paused) {
            if !on {
                continue;
            }
            let shared = Shared {
                links: &self.links,
                local: &self.local,
                claim_paused_until: &mut self.claim_paused_until,
                claude_signed_out_until: &mut self.claude_signed_out_until,
                engine_limits: &mut self.engine_limits,
                stopping,
            };
            if !paused {
                lp.draft_pass(slots, &host, &shared, lanes).await;
            }
            lp.land_drafts(&host, &shared, self.merge_wait, lanes).await;
            let drafting = lp.drafting().then(|| lp.draft.as_ref().map(|d| d.id.clone())).flatten();
            lp.live.set_drafting(&lp.id, drafting);
        }
    }

    /// Runs each new project's trial plan, once its setup checks pass. It comes after asks and drafts and before items
    /// are planned, and counts toward `concurrency` too.
    pub(super) async fn trial_passes(&mut self, on: &[bool], paused: &[bool], stopping: bool, slots: &mut u32) {
        let Some((host, checks)) = self.trial.clone() else {
            return;
        };
        *slots = slots.saturating_sub(self.projects.iter().filter(|lp| lp.trialing()).count() as u32);
        for ((lp, &on), &paused) in self.projects.iter_mut().zip(on).zip(paused) {
            if on {
                let shared = Shared {
                    links: &self.links,
                    local: &self.local,
                    claim_paused_until: &mut self.claim_paused_until,
                    claude_signed_out_until: &mut self.claude_signed_out_until,
                    engine_limits: &mut self.engine_limits,
                    stopping,
                };
                lp.trial_pass(&mut self.pool, slots, paused, &host, &checks, &shared)
                    .await;
            }
        }
    }
}
