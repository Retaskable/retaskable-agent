//! The pass's second half for one project: what its turns decided, the `tick` event and the watchdog's minute.
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::project::ProjectLoop;
use crate::backlog::Item;
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::live::{HeldLane, now_iso8601};
use crate::orchestrator::{Action, tick_payload};
use crate::telemetry::events::parse_iso_ms;
use crate::telemetry::{Event, now_ms, watchdog};

use super::Step;
use super::project::describe;

impl ProjectLoop {
    /// The pass's second half for a project whose items were read: records what its turns decided
    /// (`actions`, from `round_robin::plan`) against the computer's `pool`, and writes the `tick` event.
    pub(super) fn record(
        &mut self,
        items: Vec<Item>,
        paused: bool,
        actions: Vec<Action>,
        pool: &LanePool,
        live: Option<Option<String>>,
    ) -> Step {
        let mut states: BTreeMap<String, u32> = BTreeMap::new();
        for item in &items {
            if let Ok(Value::String(s)) = serde_json::to_value(item.status) {
                *states.entry(s).or_default() += 1;
            }
        }
        // The dry loop applies nothing, so the real runs, pool and log see only the `tick` event below.
        // The lanes held are the computer's: this project's are the ones it shows.
        let held: Vec<HeldLane> = pool
            .held_lanes()
            .filter_map(|lane| match pool.holder(lane)? {
                Holder::Item { project, item } | Holder::Play { project, item } if *project == self.id => {
                    Some(HeldLane {
                        lane,
                        title: items.iter().find(|i| &i.id == item).map(|i| i.title.clone()),
                        item_id: item.clone(),
                    })
                }
                _ => None,
            })
            .collect();
        let payload = tick_payload(paused, pool);
        let would: Vec<String> = actions.iter().map(describe).collect();
        let decision = match actions.first() {
            None if paused => "paused: would do nothing".to_string(),
            None => "would do nothing".to_string(),
            Some(Action::AskForWork) if live.is_some() => "asked for work".to_string(),
            Some(Action::AskForWork) => "would ask for work".to_string(),
            Some(Action::StartImplementer { item_id, lane }) if live.is_some() => {
                format!("started {item_id} on lane {lane}")
            }
            Some(Action::StartImplementer { item_id, lane }) => format!("would start {item_id} on lane {lane}"),
            Some(Action::NeedsHuman { .. }) => unreachable!("the plan lists no needs_human"),
        };
        self.live.ticked(&self.id, paused, now_iso8601(), held, decision);
        let mut payload = payload;
        payload.states = Some(json!(states));
        // `live` is the claim answer when this pass really applied its decisions (`claim` mode with a starter).
        payload.claim = Some(json!(live.clone().flatten().unwrap_or_else(|| "not asked".into())));
        if live.is_none() {
            payload.extra.insert("dry".into(), json!(would));
        }
        self.events.add_in(&self.id, Event::Tick(payload));
        if !would.is_empty() {
            tracing::debug!(
                "{}: tick {}: {}",
                self.id,
                if live.is_some() { "did" } else { "would" },
                would.join(", ")
            );
        }
        self.check_watchdog(paused, &actions, pool);
        Step::Ticked { would }
    }

    /// This minute's watchdog rules (docs/specs/agent-telemetry.md section 4): finds what's quietly gone
    /// wrong against the state this tick already has (`actions`, `pool`) and times it against findings
    /// from minutes before.
    fn check_watchdog(&mut self, paused: bool, actions: &[Action], pool: &LanePool) {
        let recent: Vec<watchdog::RecentEvent> = self
            .events
            .recent()
            .iter()
            .filter_map(|e| {
                let at_ms = parse_iso_ms(&e.at)?;
                let event = e.event()?;
                Some(watchdog::RecentEvent { at_ms, event })
            })
            .collect();
        let now_ms = now_ms();
        let found = watchdog::check(&watchdog::Input {
            project: &self.id,
            paused,
            runs: &self.runs,
            leases: &self.leases,
            records: &self.records,
            running: &self.running,
            pool,
            actions,
            recent: &recent,
            now_ms,
            lease_idle_minutes: self.project.tuning.lease_idle_minutes,
            idle_lanes_minutes: self.project.tuning.idle_lanes_minutes,
            no_progress_minutes: self.project.tuning.no_progress_minutes,
        });
        self.telemetry.minute(&self.id, found, now_ms, &mut self.events);
    }
}
