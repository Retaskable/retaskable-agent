//! The engine start rules in the project loop (docs/specs/engines/03-choosing-the-engine.md sections 3.2 and 3.5),
//! behind the `engine-start-rules` flag: new work only takes a lane whose own engine may start now.
use std::collections::BTreeSet;
use std::time::Instant;

use super::Shared;
use super::project::ProjectLoop;
use super::signed_out::signed_out;
use crate::engines::start_rules::{engine_now, engines_out, engines_used_here, lane_may_start, may_start};
use crate::engines::{EngineId, Role, engine_for, program_of};
use crate::lane_pool::LanePool;
use crate::orchestrator::{Action, decide_where};

/// Which engines this computer uses, and which of them are out right now.
pub(super) struct StartGate {
    used: Vec<EngineId>,
    out: BTreeSet<EngineId>,
}

impl StartGate {
    /// `None` while `engine-start-rules` (`on`) is Off.
    pub(super) fn new(on: bool, lp: &ProjectLoop, pool: &LanePool, shared: &Shared<'_>) -> Option<StartGate> {
        if !on {
            return None;
        }
        let used = engines_used_here(
            &shared.links.engine_choice,
            lp.project.engines.as_ref(),
            pool.rules().lanes,
        );
        let out = engines_out(shared.engine_limits, *shared.claude_signed_out_until, Instant::now());
        Some(StartGate { used, out })
    }

    /// Whether new work may start in `lane`: its engine, after `engine_now`, isn't out.
    pub(super) fn lane_may_start(&self, lp: &ProjectLoop, shared: &Shared<'_>, lane: u32) -> bool {
        lane_may_start(
            &shared.links.engine_choice,
            lp.project.engines.as_ref(),
            lane,
            &self.used,
            &self.out,
        )
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

    /// Whether `engine` may start now.
    pub(super) fn may_start(&self, engine: EngineId) -> bool {
        may_start(engine, &self.out)
    }

    /// Whether a fresh implementer in `lane` is held back: its engine is still out after `engine_now`.
    pub(super) fn holds(&self, lp: &ProjectLoop, shared: &Shared<'_>, lane: u32) -> bool {
        !may_start(self.engine_now(lp, shared, lane), &self.out)
    }
}

impl ProjectLoop {
    /// `decide`, with new work limited to lanes whose engine may start while the gate is on.
    pub(super) fn decide_gated(&self, pool: &LanePool, shared: &Shared<'_>, gate: Option<&StartGate>) -> Vec<Action> {
        decide_where(&self.id, false, &self.runs, pool, &self.project, |k| {
            gate.is_none_or(|g| g.lane_may_start(self, shared, k))
        })
    }

    /// Claude's sign-out holds every claim only while the start rules are Off.
    pub(super) fn claim_held_by_sign_out(shared: &Shared<'_>, gate: Option<&StartGate>) -> bool {
        gate.is_none() && signed_out(shared)
    }
}

impl ProjectLoop {
    /// Whether the start rules (On) hold a session on `lane` back: the engine it would run on is out.
    pub(super) async fn start_held(&mut self, pool: &LanePool, shared: &Shared<'_>, lane: u32) -> bool {
        let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
        gate.is_some_and(|g| g.holds(self, shared, lane))
    }
}
