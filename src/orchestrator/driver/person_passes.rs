//! The passes that aren't about an item, run in `claim` mode (from `schedule`) and in `runs` mode (from
//! `run_passes`): the chat's asks and a person's Build this. What decides where they run is [`Machine::acts`], and
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
}
