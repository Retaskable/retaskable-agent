//! Auto concurrency in the machine-wide loop (docs/specs/agent/07.1-model-tiers.md, "Auto concurrency"): one pacer
//! for the whole computer, stepped from `schedule`, and the number of sessions it allows.
use super::machine::Machine;
use crate::config::AutoConcurrency;
use crate::orchestrator::round_robin::{Candidate, plan};
use crate::pace::Pacer;

/// The pacer, its settings and the number it last gave.
pub(super) struct Auto {
    pub(super) cfg: AutoConcurrency,
    pub(super) pacer: Pacer,
    /// The number the pacer last gave; `None` before its first step.
    pub(super) allowed: Option<u32>,
    /// Whether the `auto-concurrency` flag and `autoConcurrency.enabled` are both On this pass.
    pub(super) on: bool,
    /// Whether the last pass ended with no slot left while a live, unpaused project still had work to start.
    pub(super) work_waiting: bool,
}

impl Auto {
    pub(super) fn new(cfg: AutoConcurrency) -> Self {
        Self {
            cfg,
            pacer: Pacer::default(),
            allowed: None,
            on: false,
            work_waiting: false,
        }
    }
}

impl Machine {
    /// Epoch seconds, or the test's clock.
    pub(super) fn now_secs(&self) -> i64 {
        #[cfg(test)]
        if let Some(t) = self.clock {
            return t;
        }
        crate::telemetry::now_ms() / 1000
    }

    /// How many sessions may run at once: the pacer's number with auto concurrency on, else `concurrency`.
    pub(super) fn effective_concurrency(&self) -> u32 {
        match (self.auto.on, self.auto.allowed) {
            (true, Some(n)) => n,
            (true, None) => self.concurrency.clamp(self.auto.cfg.min, self.auto.cfg.max),
            _ => self.concurrency,
        }
    }

    /// Settles the flag for this pass, feeds the pacer the Claude readings that settled since the last pass and
    /// steps it when it is due.
    pub(super) async fn pace_step(&mut self, ready: &[bool], live: &[bool]) {
        let mut on = false;
        let flags: Vec<_> = (self.projects.iter().zip(live))
            .filter(|(_, live)| **live)
            .map(|(lp, _)| lp.flags.clone())
            .collect();
        for flag in flags {
            if self.auto.cfg.enabled && flag.on("auto-concurrency").await {
                on = true;
                break;
            }
        }
        self.auto.on = on;
        let now = self.now_secs();
        for (lp, &ready) in self.projects.iter_mut().zip(ready) {
            let mut readings = std::mem::take(&mut lp.pace_readings);
            if ready && !lp.pace_checked {
                lp.pace_checked = true;
                readings.extend([lp.store.usage(), lp.store.week_usage()].into_iter().flatten());
            }
            for usage in readings {
                self.auto.pacer.record(now, &usage);
            }
        }
        if !on || !self.auto.pacer.due(now) {
            return;
        }
        let from = self.effective_concurrency();
        let pace = self
            .auto
            .pacer
            .pace(now, from, self.concurrency, self.auto.work_waiting, &self.auto.cfg);
        self.auto.allowed = Some(pace.allowed);
        if pace.allowed != from {
            tracing::info!(
                event = "auto_concurrency",
                from,
                to = pace.allowed,
                "auto concurrency: {from} -> {} sessions ({})",
                pace.allowed,
                pace.reason
            );
        }
    }

    /// With no slot left, whether one more would start something: a live, unpaused project with a lane or an item.
    pub(super) fn work_waits(&self, live: &[bool], paused: &[bool]) -> bool {
        let mut candidates: Vec<Option<Candidate>> = (self.projects.iter().zip(live).zip(paused))
            .map(|((lp, &live), &paused)| {
                (live && !paused).then(|| Candidate {
                    id: &lp.id,
                    paused,
                    runs: lp.runs.clone(),
                    cfg: &lp.project,
                    rush: Default::default(),
                })
            })
            .collect();
        plan(&self.pool, None, 1, &mut candidates)
            .0
            .iter()
            .any(|a| !a.is_empty())
    }
}
