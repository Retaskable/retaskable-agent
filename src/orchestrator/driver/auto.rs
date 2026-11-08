//! Auto concurrency in the machine-wide loop (docs/specs/agent/07.1-model-tiers.md, "Auto concurrency"): one pacer
//! for the whole computer, stepped from `schedule`, and the number of sessions it allows.
use super::machine::Machine;
use crate::config::AutoConcurrency;
use crate::orchestrator::round_robin::{Candidate, plan};
use crate::pace::Pacer;
use crate::store::MachineStore;

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
    /// Where `machine.json` is; `None` keeps nothing across a restart.
    pub(super) dir: Option<std::path::PathBuf>,
    /// Whether the saved state was looked at this run.
    restored: bool,
    /// The number and window last written.
    saved: Option<(u32, i64)>,
}

impl Auto {
    pub(super) fn new(cfg: AutoConcurrency) -> Self {
        Self {
            cfg,
            pacer: Pacer::default(),
            allowed: None,
            on: false,
            work_waiting: false,
            dir: None,
            restored: false,
            saved: None,
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
        for flag in &flags {
            if self.auto.cfg.enabled && flag.on("auto-concurrency").await {
                on = true;
                break;
            }
        }
        self.auto.on = on;
        let mut lone_week = false;
        if on {
            for flag in &flags {
                if flag.on("auto-concurrency-week").await {
                    lone_week = true;
                    break;
                }
            }
        }
        if !on {
            self.links.item_commands.set_auto(None);
        }
        let now = self.now_secs();
        if on {
            self.restore_pace(now);
        }
        for (lp, &ready) in self.projects.iter_mut().zip(ready) {
            let mut readings = std::mem::take(&mut lp.pace_readings);
            if ready && !lp.pace_checked {
                lp.pace_checked = true;
                readings.extend([lp.store.usage(), lp.store.week_usage()].into_iter().flatten());
            }
            for usage in readings {
                self.auto.pacer.record_with(now, &usage, lone_week);
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
        self.links
            .item_commands
            .set_auto(Some((pace.allowed, pace.reason.to_string())));
        self.save_pace(pace.allowed, pace.reason.to_string());
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

    /// Once, with the flag On: takes the saved number and clock back when its window hasn't reset.
    fn restore_pace(&mut self, now: i64) {
        if std::mem::replace(&mut self.auto.restored, true) {
            return;
        }
        let Some(dir) = &self.auto.dir else { return };
        let Some(saved) = MachineStore::load(dir).ok().and_then(|s| s.auto_concurrency()) else {
            return;
        };
        if self.auto.pacer.restore(now, &saved) {
            self.auto.allowed = Some(saved.allowed.clamp(self.auto.cfg.min, self.auto.cfg.max));
            self.auto.saved = Some((saved.allowed, saved.resets_at));
        }
    }

    /// Writes the pacer's state when the number or the window changed.
    fn save_pace(&mut self, allowed: u32, reason: String) {
        let Some(dir) = &self.auto.dir else { return };
        let Some(state) = self.auto.pacer.state(allowed, reason) else {
            return;
        };
        if self.auto.saved == Some((allowed, state.resets_at)) {
            return;
        }
        let saved = MachineStore::load(dir).and_then(|mut s| s.set_auto_concurrency(&state));
        match saved {
            Ok(()) => self.auto.saved = Some((allowed, state.resets_at)),
            Err(e) => tracing::warn!("auto concurrency: couldn't save the pacer: {e:#}"),
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
