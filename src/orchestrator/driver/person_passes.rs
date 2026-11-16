//! The passes that aren't about an item, run in `claim` mode (from `schedule`) and in `runs` mode (from
//! `run_passes`): the chat's asks, expert sessions, a person's Build this, drafts and the trial plan. What decides where they run is [`Machine::acts`], and
//! what they count against `concurrency` is [`Machine::sessions`].
use super::{Machine, Shared};
use crate::engines::start_rules::engines_out;
use crate::engines::{Role, engine_for, program_of};

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

    /// Takes the site's Add spec and Build this rows, written up as drafts in the store, for the `on` projects. A pause
    /// holds them back, and no session slot is used.
    pub(super) async fn wanted_passes(&mut self, on: &[bool], paused: &[bool], stopping: bool) {
        if !self.wanted {
            return;
        }
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
                lp.read_pass(&self.links).await;
                lp.wanted_pass(paused, &self.work, &self.links, stopping, &shared).await;
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
            // A paused project still tells the store how its drafts ended.
            lp.resume_final_reports(&shared).await;
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

    /// The CI watcher and the Fix button's rows, for the `on` projects: no session slot, and a pause doesn't hold the
    /// watcher back; the store names the one computer that looks. In `runs` mode a project is on only once a read of
    /// its backlog went through, since the board's branches come from it.
    pub(super) async fn ci_passes(&mut self, on: &[bool], paused: &[bool], stopping: bool) {
        let Some(host) = self.ci_watch.clone() else { return };
        let now = std::time::Instant::now();
        for ((lp, &on), &paused) in self.projects.iter_mut().zip(on).zip(paused) {
            if on {
                let token = self.links.store_token().await.ok().flatten();
                // Judging and reruns go on through a pause; a fix session or a push of main doesn't (the store asks again).
                let out = engines_out(&self.engine_limits, self.claude_signed_out_until, now);
                let engine = program_of(engine_for(
                    &self.links.engine_choice,
                    lp.project.engines.as_ref(),
                    Role::Implementer,
                    None,
                ));
                let hold = paused || out.contains(&engine);
                lp.ci_pass(&host, self.runners.is_runner(&lp.id), token, stopping, hold, now);
                // The Fix button's rows: only the runner takes them, and not while paused or signed out.
                let shared = Shared {
                    links: &self.links,
                    local: &self.local,
                    claim_paused_until: &mut self.claim_paused_until,
                    claude_signed_out_until: &mut self.claude_signed_out_until,
                    engine_limits: &mut self.engine_limits,
                    stopping,
                };
                lp.fix_pass(&host, self.runners.flag(&lp.id), paused, &self.work, &shared)
                    .await;
            }
        }
    }

    /// A person's Merge into main, for the `on` projects: no session, so no slot, and a pause doesn't hold it back.
    /// What a row names is the handler's: an item is claimed by name only where items are held (`claim` mode).
    pub(super) async fn merge_passes(&mut self, on: &[bool], stopping: bool) {
        let Some(host) = self.merge_host.clone() else { return };
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
                lp.merge_pass(&host, &shared).await;
            }
        }
    }

    /// A person's Open PR to main, for the `on` projects: no session, so no slot, and a pause doesn't hold it back.
    pub(super) async fn main_pr_passes(&mut self, on: &[bool], stopping: bool) {
        let Some(host) = self.main_pr_host.clone() else { return };
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
                lp.main_pr_pass(&host, &shared).await;
            }
        }
    }
}
