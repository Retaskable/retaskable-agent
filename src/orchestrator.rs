//! The orchestrator's tick: decides what each lane works on next (docs/specs/agent/07-orchestrator-loop.md),
//! one project at a time (`Pool.tick`/`Orchestrator.step` in Retaskable.com's `agent/src/pool.ts` and
//! `agent/src/orchestrator.ts`). The pseudocode's first half — the watchdog, the once-a-minute fetch,
//! and advancing every non-terminal item a step — needs a running session, a git checkout and the CI/
//! checks/review/merge steps to call into, none of which exist in this crate yet (those are other,
//! not-yet-built pieces: implementer-session, review-stage1/2/3, post-merge-smoke-test, ...). This
//! module is only the pseudocode's last two lines: a `queued` item (one the store already handed out,
//! `item_lifecycle::ItemState::Queued`) takes a free lane, and only once nothing local is ready to
//! start does the caller get told to ask the store for more (`claim_next`, `backlog::Client`) — kept
//! as its own action rather than called here, since that's a network round trip and this stays pure
//! and testable the same way `item_lifecycle` and `lane_pool` are. What's deliberately left out:
//! actually starting a session (implementer-session), the pause flag's real source (today it's
//! pushed from the store, backlog/06.2-operations.md's `pause`/`resume`), and `activeSessions` /
//! `concurrency` (no session pool exists yet to count).
pub mod driver;
pub mod live;

use std::collections::BTreeMap;

use crate::config::ProjectConfig;
use crate::item_lifecycle::{ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool, Want};
use crate::telemetry::{Event, EventLog, event};

/// One project's items tracked between ticks, by item id.
pub type ProjectRuns = BTreeMap<String, ItemRun>;

/// The minute summary for one project (agent-telemetry.md section 3.3's `tick`), as `Pool.minute` in
/// `agent/src/pool.ts` writes it: `status` is `paused` or `running` here. The TS agent also has `setup`
/// (no config yet) and `waiting_for_limit` (`Orchestrator.status`); neither has a source in this crate
/// yet. `sessions` is 0 because no session runs here yet. `states` and `claim` are left for the caller
/// that knows them.
pub fn tick_payload(paused: bool, pool: &LanePool) -> event::Tick {
    let busy: Vec<u32> = pool.held_lanes().collect();
    let lanes = pool.rules().lanes;
    let free = lanes.saturating_sub(busy.iter().filter(|&&k| k <= lanes).count() as u32);
    event::Tick {
        status: Some(if paused { "paused" } else { "running" }.into()),
        lanes: Some(lanes),
        busy: Some(busy),
        free: Some(free.into()),
        sessions: Some(0.into()),
        ..Default::default()
    }
}

/// What one tick decided for one project, in the order it decided them. At most one `AskForWork`
/// per tick, after every locally ready item has taken a lane (backlog/06.6-handing-out-work.md
/// section 6.6.1: a lane must be free before the store is asked at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `item_id` was `queued` and took `lane`; the caller starts its implementer session there.
    StartImplementer { item_id: String, lane: u32 },
    /// Nothing local is ready, and a lane is free: the caller asks the store (`claim_next`).
    AskForWork,
}

/// One project's tick: a paused project does nothing (section 7's "if paused: skip" skips the rest
/// of the loop too, not just this). Otherwise every `queued` item that can take a lane does, each
/// moving to `implementing` (`ItemRun::claimed`) and taking the project's `lane_taken` telemetry
/// with it; only once none of them can is a free lane offered to `claim_next` instead.
///
/// This changes `runs`, `pool` and `events` as if every action were taken. A caller that must not
/// act on the decision (the dry driver, `driver.rs`) passes scratch copies of all three and keeps
/// only what it chooses from the actions returned.
pub fn tick(
    project: &str,
    paused: bool,
    runs: &mut ProjectRuns,
    pool: &mut LanePool,
    cfg: &ProjectConfig,
    events: &mut EventLog,
) -> Vec<Action> {
    events.add_in(project, Event::Tick(tick_payload(paused, pool)));
    let mut actions = Vec::new();
    if paused {
        return actions;
    }
    while let Some((item_id, run)) = runs.iter_mut().find(|(_, r)| r.state == ItemState::Queued) {
        let item_id = item_id.clone();
        let want = Want {
            project: project.to_string(),
            item: item_id.clone(),
            chore: false,
            last_lane: pool.lane_of(project, &item_id),
            fix_queued: false,
        };
        let Some(lane) = pool.claim(&want) else { break };
        run.claimed(cfg);
        if run.state == ItemState::NeedsHuman {
            // Past its slice limit: needs_human frees the lane and skips the item
            // (05-item-lifecycle.md), rather than starting an implementer for it.
            pool.release(
                lane,
                &Holder::Item {
                    project: project.to_string(),
                    item: item_id.clone(),
                },
            );
            events.add_in(
                project,
                Event::ItemState(event::ItemState {
                    item: Some(item_id.clone()),
                    from: Some("queued".into()),
                    to: Some("needs_human".into()),
                    ..Default::default()
                }),
            );
            continue;
        }
        events.add_in(
            project,
            Event::LaneTaken(event::LaneTaken {
                item: Some(item_id.clone()),
                lane: Some(lane),
                ..Default::default()
            }),
        );
        actions.push(Action::StartImplementer { item_id, lane });
    }
    let ask_want = Want {
        project: project.to_string(),
        item: String::new(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    };
    if pool.claim_lane(&ask_want).is_some() {
        actions.push(Action::AskForWork);
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lane_pool::PoolRules;
    use crate::test_support::item_lifecycle_cfg as cfg;

    fn pool(lanes: u32) -> LanePool {
        LanePool::new(PoolRules {
            lanes,
            ..Default::default()
        })
    }

    #[test]
    fn a_paused_project_does_nothing_even_with_a_queued_item_and_a_free_lane() {
        let mut runs = ProjectRuns::from_iter([("a".to_string(), ItemRun::new())]);
        let mut pool = pool(2);
        let mut events = EventLog::new();
        let actions = tick("proj", true, &mut runs, &mut pool, &cfg(), &mut events);
        assert!(actions.is_empty());
        assert_eq!(
            runs["a"].state,
            ItemState::Queued,
            "paused never advances a queued item"
        );
    }

    #[test]
    fn a_queued_item_takes_a_free_lane_and_moves_to_implementing() {
        let mut runs = ProjectRuns::from_iter([("a".to_string(), ItemRun::new())]);
        let mut pool = pool(2);
        let mut events = EventLog::new();
        let actions = tick("proj", false, &mut runs, &mut pool, &cfg(), &mut events);
        assert_eq!(
            actions[0],
            Action::StartImplementer {
                item_id: "a".into(),
                lane: 1
            }
        );
        assert_eq!(runs["a"].state, ItemState::Implementing);
        assert_eq!(pool.lane_of("proj", "a"), Some(1));
    }

    #[test]
    fn every_queued_item_that_can_take_a_lane_does_in_one_tick() {
        let mut runs = ProjectRuns::from_iter([
            ("a".to_string(), ItemRun::new()),
            ("b".to_string(), ItemRun::new()),
            ("c".to_string(), ItemRun::new()),
        ]);
        let mut pool = pool(2);
        let mut events = EventLog::new();
        let actions = tick("proj", false, &mut runs, &mut pool, &cfg(), &mut events);
        // Only two lanes: two items start, the third is left queued with nothing offered for it
        // (both lanes are now busy, so there's nothing free to ask the store with either).
        assert_eq!(actions.len(), 2);
        let started: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                Action::StartImplementer { item_id, .. } => Some(item_id.as_str()),
                Action::AskForWork => None,
            })
            .collect();
        assert_eq!(started.len(), 2);
        let still_queued: Vec<_> = runs
            .iter()
            .filter(|(_, r)| r.state == ItemState::Queued)
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(still_queued.len(), 1);
    }

    #[test]
    fn nothing_queued_and_a_free_lane_asks_the_store() {
        let mut runs = ProjectRuns::new();
        let mut pool = pool(2);
        let mut events = EventLog::new();
        let actions = tick("proj", false, &mut runs, &mut pool, &cfg(), &mut events);
        assert_eq!(actions, vec![Action::AskForWork]);
    }

    #[test]
    fn every_lane_busy_asks_for_nothing() {
        let mut runs = ProjectRuns::new();
        let mut pool = pool(1);
        assert!(pool.take(
            1,
            crate::lane_pool::Holder::Item {
                project: "proj".into(),
                item: "other".into(),
            }
        ));
        let mut events = EventLog::new();
        let actions = tick("proj", false, &mut runs, &mut pool, &cfg(), &mut events);
        assert!(actions.is_empty());
    }

    #[test]
    fn a_queued_item_past_the_slice_limit_needs_a_human_instead_of_implementing() {
        let mut run = ItemRun::new();
        run.slice = 10;
        let mut runs = ProjectRuns::from_iter([("a".to_string(), run)]);
        let mut pool = pool(1);
        let mut events = EventLog::new();
        let actions = tick("proj", false, &mut runs, &mut pool, &cfg(), &mut events);
        // needs_human frees the lane and skips the item (05-item-lifecycle.md), so the lane it
        // briefly took is free again, and the caller is told to ask the store instead.
        assert_eq!(actions, vec![Action::AskForWork]);
        assert_eq!(runs["a"].state, ItemState::NeedsHuman);
        assert_eq!(pool.lane_of("proj", "a"), None);
        let kinds: Vec<_> = events.recent().iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds,
            vec!["tick", "item_state"],
            "no lane_taken for an item that needs a human"
        );
    }

    #[test]
    fn the_tick_event_is_logged_every_time_paused_or_not() {
        let mut runs = ProjectRuns::new();
        let mut pool = pool(3);
        let mut events = EventLog::new();
        tick("proj", true, &mut runs, &mut pool, &cfg(), &mut events);
        assert_eq!(events.recent().len(), 1);
        assert_eq!(events.recent()[0].kind(), "tick");
        assert_eq!(events.recent()[0].to_value()["lanes"], 3);
    }

    #[test]
    fn the_tick_event_says_running_or_paused_and_its_exact_json() {
        let mut runs = ProjectRuns::new();
        let mut pool = pool(3);
        assert!(pool.take(
            2,
            Holder::Item {
                project: "proj".into(),
                item: "x".into(),
            }
        ));
        let mut events = EventLog::with_now(|| 0);
        tick("proj", false, &mut runs, &mut pool, &cfg(), &mut events);
        assert_eq!(
            events.recent()[0].to_value(),
            serde_json::json!({
                "at": "1970-01-01T00:00:00.000Z",
                "kind": "tick",
                "project": "proj",
                "status": "running",
                "lanes": 3,
                "busy": [2],
                "free": 2,
                "sessions": 0
            })
        );
    }

    #[test]
    fn a_paused_projects_tick_reports_paused() {
        let mut runs = ProjectRuns::new();
        let mut pool = pool(2);
        let mut events = EventLog::with_now(|| 0);
        tick("proj", true, &mut runs, &mut pool, &cfg(), &mut events);
        assert_eq!(events.recent()[0].to_value()["status"], "paused");
    }
}
