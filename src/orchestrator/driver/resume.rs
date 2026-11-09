//! Resuming the sessions a restart or a watchdog kill left running (`resume_sessions`).
use std::time::Instant;

use super::project::{Launch, ProjectLoop, signed_out};
use super::start_gate::StartGate;
use super::{SessionStarter, Shared};
use crate::engines::start_rules::{engine_now, engines_out, engines_used_here};
use crate::engines::{EngineId, Role, engine_for, lane_engine, program_of};
use crate::lane_pool::LanePool;
use crate::orchestrator::fresh_start::{FreshStart, fresh_for_engine};
use crate::store::RunRecord;

impl ProjectLoop {
    /// The engine that can take over from `started_on` while it is at its usage limit: another one the
    /// implementer role may use here (any lane's, or the default's) that isn't limited too.
    pub(super) fn takeover(
        &self,
        pool: &LanePool,
        shared: &Shared<'_>,
        started_on: EngineId,
        any_engine: bool,
    ) -> Option<EngineId> {
        takeover_in(&self.project, pool, shared, started_on, any_engine)
    }

    /// [`Self::takeover`], except for a session that stays on the local model.
    pub(super) fn takeover_unless_local(
        &self,
        pool: &LanePool,
        shared: &Shared<'_>,
        (item_id, session, engine): (&str, &str, EngineId),
        any_engine: bool,
    ) -> Option<EngineId> {
        let local = self.stays_local(shared, item_id, session, engine);
        self.takeover(pool, shared, engine, any_engine).filter(|_| !local)
    }

    /// Keeps the data folder, where session transcripts are.
    pub(super) fn with_data_dir(mut self, dir: &std::path::Path) -> Self {
        self.data_dir = Some(dir.to_path_buf());
        self
    }

    /// Whether `session` of `item_id` is a Claude session that started on the local model while `local-limit-fallback`
    /// is in force with the model up: it resumes on the model whatever Claude's limit says (section 7.1).
    pub(super) fn stays_local(&self, shared: &Shared<'_>, item_id: &str, session: &str, engine: EngineId) -> bool {
        engine == EngineId::Claude
            && Self::limit_fallback(shared)
            && self.data_dir.as_deref().is_some_and(|dir| {
                crate::engines::transcripts::started_on(dir, &self.id, item_id, session)
                    .is_some_and(|s| s.model == crate::tiers::LOCAL)
            })
    }

    /// Resumes the sessions the first pass took back mid-run, each as its item's running step. One whose last
    /// known size is over `max_resume_context_tokens` starts fresh instead (06.2-implementer.md "A session
    /// that's grown starts fresh"), keeping its lane as-is. Left for a
    /// later pass, untouched, while Claude Code is signed out on this computer (section 7.3): resuming now
    /// would only fail the same way.
    pub(super) async fn resume_sessions(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) {
        let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
        if gate.is_none() && signed_out(shared) {
            return;
        }
        let fall_over = self.flags.on("engine-fall-over").await;
        for id in std::mem::take(&mut self.to_resume) {
            let record = self.records.get(&id).cloned();
            if let Some(RunRecord {
                lane: Some(lane),
                impl_session_id: Some(session),
                impl_session_engine,
                impl_context_tokens,
                ..
            }) = record
            {
                let lane_now = lane_engine(&shared.links.engine_choice, lane).map(program_of);
                // A session that started on the local model stays on it (section 7.1): Claude's limit doesn't hold it.
                let local = self.stays_local(shared, &id, &session, impl_session_engine);
                let takeover = self
                    .takeover(pool, shared, impl_session_engine, fall_over)
                    .filter(|_| !local);
                let reason = fresh_for_engine(impl_session_engine, lane_now, takeover);
                if let Some(gate) = gate.as_ref().filter(|_| !local) {
                    // The engine its session runs on now: the takeover's, or the one it started on.
                    let on = if self.fresh_launched.contains(&id) {
                        gate.engine_now(self, shared, lane)
                    } else {
                        match reason {
                            Some(FreshStart::LaneEngine { to, .. } | FreshStart::EngineLimited { to, .. }) => to,
                            _ => impl_session_engine,
                        }
                    };
                    if !gate.may_start(on) {
                        tracing::info!("{}: {id} waits: its engine can't start now", self.id);
                        self.to_resume.push(id);
                        continue;
                    }
                }
                if self.fresh_launched.contains(&id) {
                    // Its session never started, so there is nothing to resume: start it again.
                    tracing::info!(
                        "{}: starting {id} again on lane {lane}, its session wouldn't start",
                        self.id
                    );
                    self.launch(pool, &id, lane, Launch::Fresh, starter, shared).await;
                    continue;
                }
                let kind = match impl_context_tokens {
                    _ if let Some(reason) = reason => {
                        tracing::info!("{}: {id}'s session can't resume ({reason:?}), starting fresh", self.id);
                        self.fresh_launch(&id, lane, reason, "Carry on with the slice.")
                    }
                    Some(tokens) if tokens > u64::from(self.max_resume_context_tokens) => {
                        tracing::info!(
                            "{}: {id}'s session had grown to {tokens} tokens, starting fresh instead of resuming it",
                            self.id
                        );
                        self.fresh_launch(&id, lane, FreshStart::Grown(tokens), "Carry on with the slice.")
                    }
                    _ => {
                        tracing::info!("{}: resuming the session of {id} on lane {lane}", self.id);
                        Launch::Resume(session)
                    }
                };
                self.launch(pool, &id, lane, kind, starter, shared).await;
            }
        }
    }
}

/// [`ProjectLoop::takeover`] for a caller that holds a borrow of the loop's runs. With `any_engine` (the
/// `engine-fall-over` flag) it chooses as `engine_now` does (section 3.5): from every engine this computer runs
/// for any role or lane, and Claude's sign-out counts as out. Off, only the implementer's own engines count.
pub(super) fn takeover_in(
    project: &crate::config::ProjectConfig,
    pool: &LanePool,
    shared: &Shared<'_>,
    started_on: EngineId,
    any_engine: bool,
) -> Option<EngineId> {
    let choice = &shared.links.engine_choice;
    if any_engine {
        let now = Instant::now();
        let out = engines_out(shared.engine_limits, *shared.claude_signed_out_until, now);
        let used = engines_used_here(choice, project.engines.as_ref(), pool.rules().lanes);
        return (started_on != EngineId::Pi && out.contains(&started_on))
            .then(|| engine_now(started_on, &used, &out))
            .filter(|to| *to != started_on);
    }
    let engines: Vec<EngineId> = (1..=pool.rules().lanes)
        .map(Some)
        .chain([None])
        .map(|lane| program_of(engine_for(choice, project.engines.as_ref(), Role::Implementer, lane)))
        .collect();
    shared.engine_limits.takeover(started_on, &engines, Instant::now())
}
