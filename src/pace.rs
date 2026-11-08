//! Auto concurrency's pacing rule (docs/specs/agent/07.1-model-tiers.md, "Auto concurrency"): how many sessions the
//! 5-hour usage limit allows right now, as pure code with no clock of its own (the caller passes `now`, in epoch
//! seconds). Only Claude Code readings feed it — the caller passes only those (engines/03-choosing-the-engine.md
//! section 3: Cursor, pi and Codex stay out of the burn rate).
use std::fmt;

use serde_json::Value;

use crate::config::AutoConcurrency;
use crate::tiers::{frac, week_reading};

/// Samples older than this are ignored.
const KEEP_SECS: i64 = 30 * 60;
/// The time between steps.
const STEP_SECS: i64 = 5 * 60;
const WEEK_SECS: i64 = 7 * 24 * 3600;

/// Why `pace` gave the number it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceReason {
    NewWindow,
    TargetReached,
    AheadOnWeek,
    /// Burning slower than needed, with work waiting.
    Behind {
        from: u32,
        to: u32,
    },
    /// Burning faster than needed.
    Ahead {
        from: u32,
        to: u32,
    },
    OnPace,
    /// Burning slower than needed, but nothing is waiting for a slot.
    NoWork,
    /// No burn rate yet: leaning in one step while work waits.
    Leaning {
        from: u32,
        to: u32,
    },
    /// No five-hour reading yet.
    NoReading,
}

impl fmt::Display for PaceReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NewWindow => write!(f, "New window"),
            Self::TargetReached => write!(f, "Target reached"),
            Self::AheadOnWeek => write!(f, "Ahead on the week"),
            Self::Behind { from, to } => write!(f, "Behind pace: {from} → {to}"),
            Self::Ahead { from, to } => write!(f, "Ahead of pace: {from} → {to}"),
            Self::OnPace => write!(f, "On pace"),
            Self::NoWork => write!(f, "Behind pace, no work waiting"),
            Self::Leaning { from, to } => write!(f, "No burn rate yet: {from} → {to}"),
            Self::NoReading => write!(f, "No usage reading"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pace {
    pub allowed: u32,
    pub reason: PaceReason,
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    at: i64,
    utilization: f64,
    resets_at: i64,
}

#[derive(Debug, Default)]
pub struct Pacer {
    samples: Vec<Sample>,
    /// The weekly window's `(utilization 0–1, resetsAt)`.
    week: Option<(f64, i64)>,
    /// The five-hour `resetsAt` at the last pace.
    last_resets: Option<i64>,
    last_step: Option<i64>,
}

/// The 5-hour `(utilization 0–1, resetsAt)` of a reading, read like `tiers.rs`: `unifiedWindows.five_hour` first,
/// else the flat `utilization` next to a `five_hour` (or untyped) `resetsAt`.
fn five_hour(usage: &Value) -> Option<(f64, i64)> {
    let (u, r) = match usage.get("unifiedWindows").and_then(|w| w.get("five_hour")) {
        Some(w) => (w.get("utilization")?, w.get("resetsAt")?),
        None => {
            if usage
                .get("rateLimitType")
                .is_some_and(|t| t.as_str() != Some("five_hour"))
            {
                return None;
            }
            (usage.get("utilization")?, usage.get("resetsAt")?)
        }
    };
    Some((frac(u.as_f64()?), r.as_i64()?))
}

/// The week's `(utilization 0–1, resetsAt)`: `unifiedWindows.seven_day` and, with `lone`, also a lone
/// `rateLimitType: "seven_day"` reading (`tiers::week_reading`). A reading missing either figure gives `None`.
fn seven_day(usage: &Value, lone: bool) -> Option<(f64, i64)> {
    if !lone && usage.get("unifiedWindows")?.get("seven_day").is_none() {
        return None;
    }
    let (u, r) = week_reading(usage)?;
    Some((u?, r?))
}

/// What `.retaskable/machine.json` keeps of the pacer (`autoConcurrency`), so a restart in the same window keeps its
/// number and its 5-minute clock.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PacerState {
    pub allowed: u32,
    pub reason: String,
    /// The `resetsAt` of the window it was paced in, epoch seconds.
    pub resets_at: i64,
    /// When the pacer last stepped, epoch seconds.
    pub last_step: i64,
    /// The latest samples as `[at, utilization, resetsAt]`.
    #[serde(default)]
    pub samples: Vec<(i64, f64, i64)>,
}

const SAVED_SAMPLES: usize = 12;

impl Pacer {
    /// The state to save with the number the pacer just gave; `None` before a step in a window.
    pub fn state(&self, allowed: u32, reason: String) -> Option<PacerState> {
        let skip = self.samples.len().saturating_sub(SAVED_SAMPLES);
        Some(PacerState {
            allowed,
            reason,
            resets_at: self.last_resets?,
            last_step: self.last_step?,
            samples: (self.samples[skip..].iter())
                .map(|s| (s.at, s.utilization, s.resets_at))
                .collect(),
        })
    }

    /// Takes a saved state back when its window hasn't reset yet; says whether it did.
    pub fn restore(&mut self, now: i64, saved: &PacerState) -> bool {
        if saved.resets_at <= now {
            return false;
        }
        self.last_resets = Some(saved.resets_at);
        self.last_step = Some(saved.last_step);
        self.samples = (saved.samples.iter())
            .filter(|s| s.2 == saved.resets_at && now - s.0 <= KEEP_SECS)
            .map(|&(at, utilization, resets_at)| Sample {
                at,
                utilization,
                resets_at,
            })
            .collect();
        true
    }

    /// Takes a Claude Code `rate_limit_info` reading seen at `now`.
    pub fn record(&mut self, now: i64, usage: &Value) {
        self.record_with(now, usage, false);
    }

    /// `record`, and with `lone_week` a lone `rateLimitType: "seven_day"` reading is also read as the week.
    pub fn record_with(&mut self, now: i64, usage: &Value, lone_week: bool) {
        if let Some((utilization, resets_at)) = five_hour(usage) {
            self.samples
                .retain(|s| s.resets_at == resets_at && now - s.at <= KEEP_SECS);
            self.samples.push(Sample {
                at: now,
                utilization,
                resets_at,
            });
        }
        if let Some(week) = seven_day(usage, lone_week) {
            self.week = Some(week);
        }
    }

    fn new_window(&self) -> bool {
        match (self.last_resets, self.samples.last()) {
            (Some(last), Some(s)) => s.resets_at != last,
            _ => false,
        }
    }

    /// Whether a step is due: 5 minutes since the last one, or a new window at once.
    pub fn due(&self, now: i64) -> bool {
        self.last_step.is_none_or(|t| now - t >= STEP_SECS) || self.new_window()
    }

    /// How many sessions to allow, given the `current` number, the configured `concurrency` and whether work waits.
    pub fn pace(
        &mut self,
        now: i64,
        current: u32,
        concurrency: u32,
        work_waiting: bool,
        cfg: &AutoConcurrency,
    ) -> Pace {
        let (min, max) = (cfg.min, cfg.max);
        let new_window = self.new_window();
        let stale = self.samples.last().is_some_and(|s| now - s.at > KEEP_SECS);
        self.last_resets = self.samples.last().map(|s| s.resets_at).or(self.last_resets);
        self.last_step = Some(now);
        if new_window {
            return Pace {
                allowed: concurrency.clamp(min, max),
                reason: PaceReason::NewWindow,
            };
        }
        let current = current.clamp(min, max);
        let latest = self.samples.last().copied().filter(|_| !stale);
        if latest.is_some_and(|s| s.utilization * 100.0 >= f64::from(cfg.target_pct)) {
            return Pace {
                allowed: min,
                reason: PaceReason::TargetReached,
            };
        }
        if let Some((used, resets_at)) = self.week {
            let elapsed = 1.0 - ((resets_at - now) as f64 / WEEK_SECS as f64).clamp(0.0, 1.0);
            if (used - elapsed) * 100.0 > f64::from(cfg.week_margin_pct) {
                // The floor is the spec's, whatever `min` is: with min 3 and floor 2 the week may go to 2.
                let floor = cfg.week_floor.min(max);
                let allowed = current.saturating_sub(1).max(floor).min(current);
                return Pace {
                    allowed,
                    reason: PaceReason::AheadOnWeek,
                };
            }
        }
        let Some(latest) = latest else {
            // No reading in the last half hour: no burn rate, so lean in while work waits.
            let up = (current + 1).min(max);
            return if work_waiting {
                Pace {
                    allowed: up,
                    reason: PaceReason::Leaning { from: current, to: up },
                }
            } else {
                Pace {
                    allowed: current,
                    reason: PaceReason::NoReading,
                }
            };
        };
        let kept: Vec<Sample> = self
            .samples
            .iter()
            .copied()
            .filter(|s| s.resets_at == latest.resets_at && now - s.at <= KEEP_SECS)
            .collect();
        let up = (current + 1).min(max);
        let first = kept[0];
        if kept.len() < 2 || latest.at <= first.at {
            return if work_waiting {
                Pace {
                    allowed: up,
                    reason: PaceReason::Leaning { from: current, to: up },
                }
            } else {
                Pace {
                    allowed: current,
                    reason: PaceReason::OnPace,
                }
            };
        }
        let burn = (latest.utilization - first.utilization) / (latest.at - first.at) as f64;
        let left = (latest.resets_at - now).max(1) as f64;
        let needed = (f64::from(cfg.target_pct) / 100.0 - latest.utilization) / left;
        if burn < 0.85 * needed {
            if work_waiting {
                return Pace {
                    allowed: up,
                    reason: PaceReason::Behind { from: current, to: up },
                };
            }
            return Pace {
                allowed: current,
                reason: PaceReason::NoWork,
            };
        }
        if burn > 1.15 * needed {
            let down = current.saturating_sub(1).max(min);
            return Pace {
                allowed: down,
                reason: PaceReason::Ahead {
                    from: current,
                    to: down,
                },
            };
        }
        Pace {
            allowed: current,
            reason: PaceReason::OnPace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RESET: i64 = 100_000;

    fn cfg() -> AutoConcurrency {
        AutoConcurrency::default()
    }

    fn reading(u: f64, resets: i64) -> Value {
        json!({"unifiedWindows": {"five_hour": {"utilization": u, "resetsAt": resets}}})
    }

    /// Two samples 10 minutes apart ending at `now`; the rate between them is `rate` utilization per second.
    fn pacer(now: i64, u_now: f64, rate: f64) -> Pacer {
        let mut p = Pacer::default();
        p.record(now - 600, &reading(u_now - rate * 600.0, RESET));
        p.record(now, &reading(u_now, RESET));
        p.last_resets = Some(RESET);
        p
    }

    // At now = RESET - 10000 and 0.30 used, the needed rate is 0.45 / 10000 = 4.5e-5 per second.
    const NOW: i64 = RESET - 10_000;
    const NEEDED: f64 = 4.5e-5;

    #[test]
    fn behind_with_work_climbs_one_and_stops_at_max() {
        let mut p = pacer(NOW, 0.30, NEEDED * 0.5);
        let r = p.pace(NOW, 3, 3, true, &cfg());
        assert_eq!(
            r,
            Pace {
                allowed: 4,
                reason: PaceReason::Behind { from: 3, to: 4 }
            }
        );
        assert_eq!(r.reason.to_string(), "Behind pace: 3 → 4");
        assert_eq!(p.pace(NOW, 8, 3, true, &cfg()).allowed, 8);
    }

    #[test]
    fn behind_without_work_stays() {
        let mut p = pacer(NOW, 0.30, NEEDED * 0.5);
        assert_eq!(p.pace(NOW, 3, 3, false, &cfg()).allowed, 3);
    }

    #[test]
    fn ahead_drops_one_and_stops_at_min() {
        let mut p = pacer(NOW, 0.30, NEEDED * 1.5);
        assert_eq!(p.pace(NOW, 3, 3, true, &cfg()).allowed, 2);
        assert_eq!(p.pace(NOW, 1, 3, true, &cfg()).allowed, 1);
    }

    #[test]
    fn on_pace_is_unchanged() {
        let mut p = pacer(NOW, 0.30, NEEDED);
        let r = p.pace(NOW, 3, 3, true, &cfg());
        assert_eq!((r.allowed, r.reason), (3, PaceReason::OnPace));
    }

    #[test]
    fn ahead_on_the_week_drops_even_when_behind_and_keeps_the_floor() {
        let mut p = pacer(NOW, 0.30, NEEDED * 0.5);
        // Half the week gone, 80% used.
        p.record(
            NOW,
            &json!({"unifiedWindows": {"seven_day": {"utilization": 0.8, "resetsAt": NOW + WEEK_SECS / 2}}}),
        );
        let r = p.pace(NOW, 5, 5, true, &cfg());
        assert_eq!((r.allowed, r.reason), (4, PaceReason::AheadOnWeek));
        assert_eq!(p.pace(NOW, 2, 5, true, &cfg()).allowed, 2);
        let low_max = AutoConcurrency { max: 1, ..cfg() };
        assert_eq!(p.pace(NOW, 1, 1, true, &low_max).allowed, 1);
        // Within the margin it isn't ahead.
        p.record(
            NOW,
            &json!({"unifiedWindows": {"seven_day": {"utilization": 0.55, "resetsAt": NOW + WEEK_SECS / 2}}}),
        );
        assert_eq!(p.pace(NOW, 5, 5, true, &cfg()).allowed, 6);
    }

    #[test]
    fn a_lone_seven_day_reading_is_the_week_only_with_the_flag_on() {
        let lone = |u: f64| json!({"rateLimitType": "seven_day", "utilization": u, "resetsAt": NOW + WEEK_SECS / 2});
        let mut p = pacer(NOW, 0.30, NEEDED * 0.5);
        p.record_with(NOW, &lone(0.8), true);
        assert_eq!(p.samples.len(), 2, "a lone weekly reading is no 5-hour sample");
        let r = p.pace(NOW, 5, 5, true, &cfg());
        assert_eq!((r.allowed, r.reason), (4, PaceReason::AheadOnWeek));
        assert_eq!(p.pace(NOW, 2, 5, true, &cfg()).allowed, 2);
        p.record_with(NOW, &json!({"rateLimitType": "seven_day", "status": "allowed"}), true);
        assert_eq!(
            p.pace(NOW, 5, 5, true, &cfg()).reason,
            PaceReason::AheadOnWeek,
            "status only keeps the week"
        );
        p.record_with(NOW, &lone(0.55), true);
        assert_eq!(p.pace(NOW, 5, 5, true, &cfg()).allowed, 6, "within the margin");
        let mut off = pacer(NOW, 0.30, NEEDED * 0.5);
        off.record_with(NOW, &lone(0.8), false);
        assert_eq!(off.pace(NOW, 5, 5, true, &cfg()).allowed, 6, "flag Off ignores it");
    }

    #[test]
    fn a_new_window_returns_to_concurrency_at_once() {
        let mut p = pacer(NOW, 0.30, NEEDED);
        p.pace(NOW, 3, 3, true, &cfg());
        assert!(!p.due(NOW + 60));
        p.record(NOW + 60, &reading(0.01, RESET + 18_000));
        assert!(p.due(NOW + 60));
        let r = p.pace(NOW + 60, 7, 4, true, &cfg());
        assert_eq!((r.allowed, r.reason), (4, PaceReason::NewWindow));
        assert_eq!(p.samples.len(), 1);
        let mut p = pacer(NOW, 0.30, NEEDED);
        p.pace(NOW, 3, 3, true, &cfg());
        p.record(NOW + 60, &reading(0.01, RESET + 18_000));
        assert_eq!(p.pace(NOW + 60, 3, 20, true, &cfg()).allowed, 8);
    }

    #[test]
    fn due_after_five_minutes() {
        let mut p = pacer(NOW, 0.30, NEEDED);
        assert!(p.due(NOW));
        p.pace(NOW, 3, 3, true, &cfg());
        assert!(!p.due(NOW + 299));
        assert!(p.due(NOW + 300));
    }

    #[test]
    fn no_burn_rate_leans_in_one_step_while_work_waits() {
        let mut p = Pacer::default();
        p.record(NOW, &reading(0.05, RESET));
        assert_eq!(
            p.pace(NOW, 2, 2, true, &cfg()).reason,
            PaceReason::Leaning { from: 2, to: 3 }
        );
        assert_eq!(p.pace(NOW, 3, 2, true, &cfg()).allowed, 4);
        assert_eq!(p.pace(NOW, 3, 2, false, &cfg()).allowed, 3);
        assert_eq!(p.pace(NOW, 8, 2, true, &cfg()).allowed, 8);
    }

    #[test]
    fn a_stale_newest_sample_does_not_panic() {
        let mut p = Pacer::default();
        p.record(NOW, &reading(0.05, RESET));
        let r = p.pace(NOW + 31 * 60, 2, 2, true, &cfg());
        assert_eq!(r.reason, PaceReason::Leaning { from: 2, to: 3 });
        assert_eq!(p.pace(NOW + 31 * 60, 2, 2, false, &cfg()).reason, PaceReason::NoReading);
    }

    #[test]
    fn the_week_floor_may_go_below_min() {
        let mut p = Pacer::default();
        p.record(
            NOW,
            &json!({"unifiedWindows": {"seven_day": {"utilization": 0.9, "resetsAt": NOW + WEEK_SECS / 2}}}),
        );
        let c = AutoConcurrency { min: 3, ..cfg() };
        assert_eq!(p.pace(NOW, 3, 3, true, &c).allowed, 2);
    }

    #[test]
    fn target_reached_drops_to_min() {
        let mut p = pacer(NOW, 0.75, NEEDED);
        let r = p.pace(NOW, 6, 6, true, &cfg());
        assert_eq!((r.allowed, r.reason), (1, PaceReason::TargetReached));
    }

    #[test]
    fn old_and_earlier_window_samples_are_ignored() {
        let mut p = Pacer::default();
        p.record(NOW - 3600, &reading(0.0, RESET));
        p.record(NOW, &reading(0.05, RESET));
        assert_eq!(
            p.pace(NOW, 2, 2, true, &cfg()).reason,
            PaceReason::Leaning { from: 2, to: 3 }
        );
        let mut p = Pacer::default();
        p.record(NOW - 600, &reading(0.9, RESET - 18_000));
        p.record(NOW, &reading(0.05, RESET));
        assert_eq!(p.samples.len(), 1);
    }

    #[test]
    fn percentage_and_flat_shapes_are_read() {
        let mut p = Pacer::default();
        p.record(
            NOW,
            &json!({"utilization": 42, "resetsAt": RESET, "rateLimitType": "five_hour"}),
        );
        assert!((p.samples[0].utilization - 0.42).abs() < 1e-9);
        p.record(
            NOW,
            &json!({"utilization": 0.9, "resetsAt": RESET, "rateLimitType": "seven_day"}),
        );
        assert_eq!(p.samples.len(), 1);
        p.record(NOW, &reading(55.0, RESET));
        assert!((p.samples[1].utilization - 0.55).abs() < 1e-9);
    }
}
