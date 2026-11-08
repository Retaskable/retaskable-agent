//! Claiming work from the shared backlog: one `claim_next` at a time, each handed-out item started at once.
use serde_json::json;

use super::Shared;
use super::project::{ProjectLoop, SessionStarter};
use super::start_gate::StartGate;
use crate::backlog::{ClaimNextAgent, ClaimNextAnswer};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::LanePool;
use crate::orchestrator::Action;
use crate::telemetry::event::{self, Event};

impl ProjectLoop {
    /// Asks the store for work while a lane is free and a session slot is left (`slots`, used up as work
    /// starts), and starts what it hands out. The tick's `claim`, the last answer: the item id, `none` or
    /// `failed`; `None` when it wasn't asked (no slot or lane, the guard, an own failure's claim pause,
    /// Claude signed out on this computer, or no link).
    pub(super) async fn claim(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) -> Option<String> {
        if shared
            .claim_paused_until
            .is_some_and(|until| std::time::Instant::now() < until)
            || shared.stopping
        {
            return None;
        }
        let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
        if Self::claim_held_by_sign_out(shared, gate.as_ref()) {
            return None;
        }
        let caps_on = self.flags.on("model-concurrency-caps").await;
        let mut last = None;
        // At most once per lane, so a store that keeps handing out work no lane takes can't hold the pass.
        for _ in 0..pool.rules().lanes {
            let model_full = self.model_full(starter, caps_on);
            let asks = self
                .decide_gated(pool, shared, gate.as_ref(), model_full)
                .contains(&Action::AskForWork);
            if *slots == 0 || !asks {
                break;
            }
            let Some((answer, more)) = self.claim_once(pool, slots, starter, shared, caps_on).await else {
                break;
            };
            last = Some(answer);
            if !more {
                break;
            }
        }
        last
    }

    /// One `claim_next`, with its `claim` event: the answer and whether to ask again (something was handed out).
    pub(super) async fn claim_once(
        &mut self,
        pool: &mut LanePool,
        slots: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
        caps_on: bool,
    ) -> Option<(String, bool)> {
        // Asked before every claim, so a TS agent started since is noticed at once.
        if let Some(guard) = self.guard.clone()
            && let Some(why) = guard.refusal().await
        {
            tracing::error!("{}: not claiming: {why}", self.id);
            return None;
        }
        let repo = self.repo?;
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                return None;
            }
        };
        let agent = ClaimNextAgent {
            machine: shared.links.machine.clone(),
            environment: shared.links.environment.clone(),
            base_branch: self.project.base_branch.clone(),
            tags: shared.links.tags.clone(),
        };
        let (answer, more) = match self.client.claim_next(repo, Some(&token), &agent).await {
            Err(e) => {
                self.store_error("claim_next", &e);
                ("failed".to_string(), false)
            }
            Ok(ClaimNextAnswer::Nothing) => ("none".to_string(), false),
            // Handed out again while this computer still has it: it isn't taken twice.
            Ok(ClaimNextAnswer::Item { item, .. }) if self.runs.contains_key(&item.id) => {
                tracing::warn!(
                    "{}: the store handed out {} again, which this computer has",
                    self.id,
                    item.id
                );
                (item.id, false)
            }
            Ok(ClaimNextAnswer::Item { item, token }) => {
                self.take_lease(&item, token, ItemState::Queued);
                let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
                let model_full = self.model_full(starter, caps_on);
                let actions = self.decide_gated(pool, shared, gate.as_ref(), model_full);
                *slots = slots.saturating_sub(self.apply_starts(&actions, pool, starter, shared).await);
                // A start refused hands the item back; asking again now could only be handed it again.
                let kept = self.runs.contains_key(&item.id);
                (item.id, kept)
            }
        };
        self.events.add_in(
            &self.id,
            Event::Claim(event::Claim {
                answer: Some(json!(answer)),
                ..Default::default()
            }),
        );
        Some((answer, more))
    }
}
