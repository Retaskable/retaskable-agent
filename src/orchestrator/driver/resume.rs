//! Resuming the sessions a restart or a watchdog kill left running (`resume_sessions`).
use std::time::Instant;

use super::project::{Launch, ProjectLoop, signed_out};
use super::{SessionStarter, Shared};
use crate::engines::{EngineId, Role, engine_for, lane_engine, program_of};
use crate::item_lifecycle::ItemState;
use crate::lane_pool::LanePool;
use crate::orchestrator::fresh_start::{FreshStart, fresh_for_engine};
use crate::store::RunRecord;

impl ProjectLoop {
    /// The engine that can take over from `started_on` while it is at its usage limit: another one the
    /// implementer role may use here (any lane's, or the default's) that isn't limited too.
    pub(super) fn takeover(&self, pool: &LanePool, shared: &Shared<'_>, started_on: EngineId) -> Option<EngineId> {
        let choice = &shared.links.engine_choice;
        let engines: Vec<EngineId> = (1..=pool.rules().lanes)
            .map(Some)
            .chain([None])
            .map(|lane| {
                program_of(engine_for(
                    choice,
                    self.project.engines.as_ref(),
                    Role::Implementer,
                    lane,
                ))
            })
            .collect();
        shared.engine_limits.takeover(started_on, &engines, Instant::now())
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
        if signed_out(shared) {
            return;
        }
        let now = Instant::now();
        let due: Vec<String> = (self.silenced_until.iter())
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            self.silenced_until.remove(&id);
            if self.runs.get(&id).is_some_and(|r| r.state == ItemState::Implementing) && !self.running.busy(&id) {
                self.to_resume.push(id);
            }
        }
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
                let takeover = self.takeover(pool, shared, impl_session_engine);
                let kind = match impl_context_tokens {
                    _ if let Some(reason) = fresh_for_engine(impl_session_engine, lane_now, takeover) => {
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
