//! Each project's watchdog state: how long a rule's finding has held, and the `anomaly` events that start
//! and end as it crosses (or falls back under) its threshold (docs/specs/agent-telemetry.md section 4).
//! [`watchdog`](super::watchdog) only finds a condition fresh every minute; this is what remembers when
//! one was first seen, so the same id keeps its own clock across minutes (and starts a new one after a
//! gap, e.g. a restart).
use std::collections::{BTreeMap, BTreeSet};

use crate::telemetry::Event;
use crate::telemetry::event::Anomaly;
use crate::telemetry::events::EventLog;
use crate::telemetry::watchdog::Found;

/// A started anomaly: the fields of its `start` event, and `since` (epoch ms of the first check that saw it).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAnomaly {
    pub id: String,
    pub rule: &'static str,
    pub item: Option<String>,
    pub text: String,
    pub details: serde_json::Value,
    pub since_ms: i64,
}

struct Open {
    first_seen_ms: i64,
    /// Set once it has crossed its threshold and written a `start` event.
    started: Option<OpenAnomaly>,
}

/// One project's watchdog state, kept for as long as its `ProjectLoop` runs.
#[derive(Default)]
pub struct Telemetry {
    open: BTreeMap<String, Open>,
}

impl Telemetry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The anomalies that have started and not ended, oldest `since` first, ties by `id`. Not saved: a new
    /// `Telemetry` starts empty.
    pub fn open_anomalies(&self) -> Vec<OpenAnomaly> {
        let mut all: Vec<OpenAnomaly> = self.open.values().filter_map(|o| o.started.clone()).collect();
        all.sort_by(|a, b| a.since_ms.cmp(&b.since_ms).then_with(|| a.id.cmp(&b.id)));
        all
    }

    /// This minute's findings: starts an anomaly once its threshold passes, ends one no longer found, and
    /// keeps timing the rest. Writes `anomaly` events to `log` and a line to the console on each start.
    pub fn minute(&mut self, project_id: &str, found: Vec<Found>, now_ms: i64, log: &mut EventLog) {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for f in found {
            seen.insert(f.id.clone());
            let open = self.open.entry(f.id.clone()).or_insert(Open {
                first_seen_ms: now_ms,
                started: None,
            });
            let held_minutes = (now_ms - open.first_seen_ms) / 60_000;
            if open.started.is_some() || held_minutes < i64::from(f.threshold_minutes) {
                continue;
            }
            open.started = Some(OpenAnomaly {
                id: f.id.clone(),
                rule: f.rule,
                item: f.item.clone(),
                text: f.text.clone(),
                details: f.details.clone(),
                since_ms: open.first_seen_ms,
            });
            log.add_in(
                project_id,
                Event::Anomaly(Anomaly {
                    item: f.item.clone(),
                    phase: Some("start".into()),
                    id: Some(f.id.clone()),
                    rule: Some(f.rule.into()),
                    text: Some(f.text.clone()),
                    details: Some(f.details.clone()),
                    ..Default::default()
                }),
            );
            tracing::warn!(target: "watchdog", "{project_id}: {}", f.text);
        }
        let ended: Vec<String> = self.open.keys().filter(|id| !seen.contains(*id)).cloned().collect();
        for id in ended {
            let Some(open) = self.open.remove(&id) else { continue };
            if open.started.is_none() {
                continue;
            }
            log.add_in(
                project_id,
                Event::Anomaly(Anomaly {
                    phase: Some("end".into()),
                    id: Some(id),
                    ms: Some((now_ms - open.first_seen_ms) as f64),
                    ..Default::default()
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests;
