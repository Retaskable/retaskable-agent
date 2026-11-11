//! The engine start rules in the project loop (docs/specs/engines/03-choosing-the-engine.md sections 3.2 and 3.5),
//! new work only takes a lane whose own engine may start now.
use std::collections::BTreeSet;
use std::time::Instant;

use super::Shared;
use super::project::ProjectLoop;
use super::signed_out::signed_out;
use crate::engines::start_rules::{engine_now, engines_out, engines_used_here, may_start};
use crate::engines::{EngineId, Role, engine_for, program_of};
use crate::lane_pool::LanePool;
use crate::orchestrator::{Action, decide_where};

/// Which engines this computer uses, and which of them are out right now.
pub(super) struct StartGate {
    used: Vec<EngineId>,
    out: BTreeSet<EngineId>,
    /// The limit fallback (see [`ProjectLoop::limit_fallback`]): Claude being limited doesn't hold its lanes.
    fallback: bool,
}

impl StartGate {
    pub(super) fn new(lp: &ProjectLoop, pool: &LanePool, shared: &Shared<'_>) -> StartGate {
        StartGate::for_lanes(lp, pool.rules().lanes, shared)
    }

    /// `new`, for a pass that knows the computer's lane count but holds no pool.
    pub(super) fn for_lanes(lp: &ProjectLoop, lanes: u32, shared: &Shared<'_>) -> StartGate {
        let used = engines_used_here(&shared.links.engine_choice, lp.project.engines.as_ref(), lanes);
        let out = engines_out(shared.engine_limits, *shared.claude_signed_out_until, Instant::now());
        let fallback = ProjectLoop::limit_fallback(shared);
        StartGate { used, out, fallback }
    }

    /// Whether new work may start in `lane`: its engine, after `engine_now`, isn't out.
    pub(super) fn lane_may_start(&self, lp: &ProjectLoop, shared: &Shared<'_>, lane: u32) -> bool {
        !self.holds(lp, shared, lane)
    }

    /// The engine a fresh session in `lane` starts on.
    pub(super) fn engine_now(&self, lp: &ProjectLoop, shared: &Shared<'_>, lane: u32) -> EngineId {
        let chosen = program_of(engine_for(
            &shared.links.engine_choice,
            lp.project.engines.as_ref(),
            Role::Implementer,
            Some(lane),
        ));
        engine_now(chosen, &self.used, &self.out)
    }

    /// What a new session of `role` does now: `None` while its engine is out; else `Some(engine)`, the one to start
    /// on when that isn't the role's chosen one. Only a fresh session (`switch`) moves to `engine_now`; a
    /// resume keeps the engine it ran on and waits while that is out.
    pub(super) fn role_start(
        &self,
        lp: &ProjectLoop,
        shared: &Shared<'_>,
        role: Role,
        switch: bool,
    ) -> Option<Option<EngineId>> {
        let chosen = program_of(engine_for(
            &shared.links.engine_choice,
            lp.project.engines.as_ref(),
            role,
            None,
        ));
        let now = if switch {
            engine_now(chosen, &self.used, &self.out)
        } else {
            chosen
        };
        self.may_start_fresh(now).then_some((now != chosen).then_some(now))
    }

    /// Whether `engine` may start now.
    pub(super) fn may_start(&self, engine: EngineId) -> bool {
        may_start(engine, &self.out)
    }

    /// Whether a fresh implementer in `lane` is held back: its engine is still out after `engine_now`.
    pub(super) fn holds(&self, lp: &ProjectLoop, shared: &Shared<'_>, lane: u32) -> bool {
        let engine = self.engine_now(lp, shared, lane);
        !self.may_start_fresh(engine)
    }

    /// Whether a fresh session may start on `engine`: it isn't out, or it is Claude held only by its usage limit
    /// with the fallback in force (the session then starts on the local model).
    fn may_start_fresh(&self, engine: EngineId) -> bool {
        may_start(engine, &self.out) || (self.fallback && engine == EngineId::Claude)
    }
}

impl ProjectLoop {
    /// `decide`, with new work limited to lanes whose engine may start and, with `model_full`
    /// (the model cap), to lanes that don't run a Claude implementer.
    pub(super) fn decide_gated(
        &self,
        pool: &LanePool,
        shared: &Shared<'_>,
        gate: &StartGate,
        model_full: bool,
    ) -> Vec<Action> {
        decide_where(&self.id, false, &self.runs, pool, &self.project, |k| {
            gate.lane_may_start(self, shared, k) && !(model_full && self.runs_claude(shared, gate, k))
        })
    }

    /// Whether a fresh implementer in `lane` would run on Claude.
    pub(super) fn runs_claude(&self, shared: &Shared<'_>, gate: &StartGate, lane: u32) -> bool {
        gate.engine_now(self, shared, lane) == EngineId::Claude
    }

    /// Whether the starter says the model a fresh implementer would run on has no slot free: Claude lanes then claim
    /// and start nothing new this pass.
    pub(super) fn model_full(&self, starter: &dyn super::project::SessionStarter) -> bool {
        !starter.implementer_may_start(self.claude_starts)
    }
}

impl ProjectLoop {
    /// Whether the start rules hold a session on `lane` back: the engine it would run on is out.
    pub(super) fn start_held(&self, pool: &LanePool, shared: &Shared<'_>, lane: u32) -> bool {
        StartGate::new(self, pool, shared).holds(self, shared, lane)
    }
}

impl ProjectLoop {
    /// Section 7.2's fallback: with the model up, Claude being limited (not signed
    /// out) holds no fresh work for a Claude lane, which starts on the local model.
    pub(super) fn limit_fallback(shared: &Shared<'_>) -> bool {
        shared.local.ladder() && !signed_out(shared)
    }

    /// Whether Claude is limited now with the fallback in force and the one local slot already has a session
    /// queued for it (or this pass started one): Claude lanes claim nothing more.
    pub(super) fn local_queue_full(&self, shared: &Shared<'_>) -> bool {
        Self::limit_fallback(shared)
            && shared.engine_limits.is_limited(EngineId::Claude, Instant::now())
            && (shared.local.waiting() > 0 || self.claude_starts > 0)
    }

    /// Tells the live state when Claude's limit lifts, while `start_blocked` says the usage limit or the weekly
    /// cutoff holds new work; otherwise clears it.
    pub(super) fn report_limit(
        &self,
        cant_start: Option<&str>,
        limits: &crate::orchestrator::fresh_start::EngineLimits,
    ) {
        let now = Instant::now();
        let resets = cant_start
            .filter(|c| *c == "usage limit")
            .and_then(|_| limits.limited_until(EngineId::Claude, now));
        self.live.set_limit(&self.id, resets);
    }
}

/// One line saying what an action would do, for the dry-run log.
pub(super) fn describe(action: &Action) -> String {
    match action {
        Action::StartImplementer { item_id, lane } => format!("start_implementer {item_id} in lane {lane}"),
        Action::NeedsHuman { item_id } => format!("needs_human {item_id}"),
        Action::AskForWork => "ask_for_work".into(),
    }
}
