//! Plain rules checked once a minute against a project's state (docs/specs/agent-telemetry.md section 4).
//! Pure: a rule only looks at [`Input`] and names what it finds. Nothing here decides anything or times
//! how long a condition has held — that debounce (and turning a finding into a started or ended `anomaly`
//! event) is [`super::telemetry::Telemetry`]'s job, since a rule is re-run fresh every minute.
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::item_lifecycle::ItemState;
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::Action;
use crate::orchestrator::steps::{Running, needs_lane, state_name};
use crate::store::RunRecord;
use crate::telemetry::event::Event;

/// `LEASE_IDLE_MS` default (minutes); a project's `tuning.leaseIdleMinutes` overrides it.
pub const LEASE_IDLE_MINUTES: u32 = 10;
/// `IDLE_LANES_MS` default (minutes); a project's `tuning.idleLanesMinutes` overrides it.
pub const IDLE_LANES_MINUTES: u32 = 5;
/// `NO_PROGRESS_MS` default (minutes); a project's `tuning.noProgressMinutes` overrides it.
pub const NO_PROGRESS_MINUTES: u32 = 20;
/// `LANE_MISMATCH_MS`: a lane the pool holds disagreeing with an item's own record must hold this long
/// before it counts (a start fills the pool before the record catches up).
pub const LANE_MISMATCH_MINUTES: u32 = 2;
/// `REPEAT_ERRORS` in `REPEAT_WINDOW_MS`.
pub const REPEAT_ERRORS: usize = 3;
pub const REPEAT_WINDOW_MINUTES: i64 = 30;

/// One rule's finding this minute: an anomaly it sees right now, named so the same condition keeps the
/// same id tick to tick. `threshold_minutes` is how long it must hold, continuously, before it is reported
/// (0 for a condition that is already the whole threshold, e.g. 3 errors in 30 minutes).
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub id: String,
    pub rule: &'static str,
    pub item: Option<String>,
    pub text: String,
    pub details: Value,
    pub threshold_minutes: u32,
}

/// A recent log line, as the rules that scan the event log need it: when (epoch ms) and what.
#[derive(Debug, Clone, Copy)]
pub struct RecentEvent<'a> {
    pub at_ms: i64,
    pub event: &'a Event,
}

/// What a minute's check is given: this project's in-memory state, nothing else (section 4: "No other I/O").
pub struct Input<'a> {
    pub project: &'a str,
    pub paused: bool,
    pub runs: &'a BTreeMap<String, crate::item_lifecycle::ItemRun>,
    pub leases: &'a BTreeMap<String, String>,
    pub records: &'a BTreeMap<String, RunRecord>,
    pub running: &'a Running,
    pub pool: &'a LanePool,
    pub actions: &'a [Action],
    pub recent: &'a [RecentEvent<'a>],
    pub now_ms: i64,
    pub lease_idle_minutes: u32,
    pub idle_lanes_minutes: u32,
    pub no_progress_minutes: u32,
}

/// Every rule's findings this minute.
pub fn check(input: &Input) -> Vec<Found> {
    let mut found = Vec::new();
    lease_idle(input, &mut found);
    double_lane(input, &mut found);
    idle_lanes(input, &mut found);
    repeat_error(input, &mut found);
    no_progress(input, &mut found);
    found
}

/// This agent holds a lease on an item stuck at `needs_human` (the stale "needs a person" stop of
/// docs/specs/agent-telemetry.md section 1's first example). The other states a lease can idle in
/// (`implementing`'s session, a running step, or simply waiting for a lane) aren't flagged: a session's own
/// staleness is the separate `tuning.sessionQuietMinutes` watchdog's job, and a queued item waiting for a
/// free lane is normal, not an anomaly.
fn lease_idle(input: &Input, out: &mut Vec<Found>) {
    for item_id in input.leases.keys() {
        let Some(run) = input.runs.get(item_id) else { continue };
        if run.state != ItemState::NeedsHuman {
            continue;
        }
        out.push(Found {
            id: format!("lease_idle:{item_id}"),
            rule: "lease_idle",
            item: Some(item_id.clone()),
            text: format!("{item_id} has held its lease at needs_human with nothing running"),
            details: json!({ "state": state_name(run.state) }),
            threshold_minutes: input.lease_idle_minutes,
        });
    }
}

/// An item held in more than one lane at once, or whose own record names a lane the pool doesn't agree it
/// holds (a start fills the pool before the record is written, so a brief mismatch is normal).
fn double_lane(input: &Input, out: &mut Vec<Found>) {
    let mut lanes_by_item: BTreeMap<&str, Vec<u32>> = BTreeMap::new();
    for lane in input.pool.held_lanes() {
        if let Some(Holder::Item { project, item }) = input.pool.holder(lane)
            && project == input.project
        {
            lanes_by_item.entry(item.as_str()).or_default().push(lane);
        }
    }
    let mut items: BTreeSet<&str> = lanes_by_item.keys().copied().collect();
    items.extend(input.records.keys().map(String::as_str));
    for item_id in items {
        let lanes = lanes_by_item.get(item_id).cloned().unwrap_or_default();
        let recorded = input.records.get(item_id).and_then(|r| r.lane);
        let doubled = lanes.len() > 1;
        let mismatched = recorded.is_some_and(|l| !lanes.contains(&l));
        if !doubled && !mismatched {
            continue;
        }
        let (threshold_minutes, text) = if doubled {
            (0, format!("{item_id} is in lanes {lanes:?} at once"))
        } else {
            (
                LANE_MISMATCH_MINUTES,
                format!("{item_id}'s record names lane {recorded:?}, but the pool holds {lanes:?} for it"),
            )
        };
        out.push(Found {
            id: format!("double_lane:{item_id}"),
            rule: "double_lane",
            item: Some(item_id.to_string()),
            text,
            details: json!({ "lanes": lanes, "recorded_lane": recorded }),
            threshold_minutes,
        });
    }
}

/// Lanes sit free while this project has work a tick would start or ask the store for.
fn idle_lanes(input: &Input, out: &mut Vec<Found>) {
    if input.paused {
        return;
    }
    let held = input.pool.held_lanes().count() as u32;
    let limit = input.pool.limit();
    let free = limit.saturating_sub(held);
    let work_ready = input
        .actions
        .iter()
        .any(|a| matches!(a, Action::StartImplementer { .. } | Action::AskForWork));
    if free == 0 || !work_ready {
        return;
    }
    out.push(Found {
        id: "idle_lanes".to_string(),
        rule: "idle_lanes",
        item: None,
        text: format!("{free} of {limit} lanes free while work is ready to start"),
        details: json!({ "free": free, "limit": limit }),
        threshold_minutes: input.idle_lanes_minutes,
    });
}

/// The same error, numbers stripped, for the same item 3 or more times in the last 30 minutes; or any
/// `host_call_undefined` in that window, at once (a programming error, never worth waiting on).
fn repeat_error(input: &Input, out: &mut Vec<Found>) {
    let since = input.now_ms - REPEAT_WINDOW_MINUTES * 60_000;
    let mut by_item: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    let mut host_call_undefined = false;
    for e in input.recent.iter().filter(|e| e.at_ms >= since) {
        match e.event {
            Event::Error(err) if err.item.is_some() => {
                let item = err.item.as_deref().unwrap_or_default();
                let text = normalize(err.error.as_deref().unwrap_or_default());
                *by_item.entry(item).or_default().entry(text).or_default() += 1;
            }
            Event::HostCallUndefined(_) => host_call_undefined = true,
            _ => {}
        }
    }
    for (item_id, errors) in by_item {
        for (text, count) in errors {
            if count >= REPEAT_ERRORS {
                out.push(Found {
                    id: format!("repeat_error:{item_id}"),
                    rule: "repeat_error",
                    item: Some(item_id.to_string()),
                    text: format!("{item_id} has failed {count} times in {REPEAT_WINDOW_MINUTES} min: {text}"),
                    details: json!({ "count": count, "error": text }),
                    threshold_minutes: 0,
                });
            }
        }
    }
    if host_call_undefined {
        out.push(Found {
            id: "repeat_error:host_call_undefined".to_string(),
            rule: "repeat_error",
            item: None,
            text: "a gh or glab call ran with an undefined argument".to_string(),
            details: Value::Null,
            threshold_minutes: 0,
        });
    }
}

/// An item in `checking`, `reviewing` or `merging` with nothing running for it and, for the two that need a
/// lane, holding one (so it isn't simply waiting its turn for a free one).
fn no_progress(input: &Input, out: &mut Vec<Found>) {
    if input.paused {
        return;
    }
    for (item_id, run) in input.runs {
        if !matches!(
            run.state,
            ItemState::Checking | ItemState::Reviewing | ItemState::Merging
        ) {
            continue;
        }
        if input.running.busy(item_id) {
            continue;
        }
        if needs_lane(run.state) && input.pool.lane_of(input.project, item_id).is_none() {
            continue;
        }
        out.push(Found {
            id: format!("no_progress:{item_id}"),
            rule: "no_progress",
            item: Some(item_id.clone()),
            text: format!("{item_id} is {} with nothing running", state_name(run.state)),
            details: json!({ "state": state_name(run.state) }),
            threshold_minutes: input.no_progress_minutes,
        });
    }
}

/// Strips digits (ids, hashes, PR numbers) so the same error at different counts or ids still groups
/// together, the way 05-item-lifecycle's retry count or a PR number would otherwise make every line unique.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_digit = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !last_was_digit {
                out.push('#');
            }
            last_was_digit = true;
        } else {
            out.push(ch);
            last_was_digit = false;
        }
    }
    out
}

#[cfg(test)]
mod tests;
