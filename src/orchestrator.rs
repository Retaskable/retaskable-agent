//! The orchestrator's tick: decides what each lane works on next (docs/specs/agent/07-orchestrator-loop.md),
//! one project at a time (`Pool.tick`/`Orchestrator.step` in Retaskable.com's `agent/src/pool.ts` and
//! `agent/src/orchestrator.ts`). The pseudocode's first half — the watchdog, the once-a-minute fetch,
//! and advancing every non-terminal item a step — needs a running session, a git checkout and the CI/
//! checks/review/merge steps to call into, none of which exist in this crate yet (those are other,
//! not-yet-built pieces: implementer-session, review-stage1/2/3, post-merge-smoke-test, ...). This
//! module is only the pseudocode's last two lines: a `queued` item (one the store already handed out,
//! `item_lifecycle::ItemState::Queued`) takes a free lane, and only once nothing local is ready to
//! start does the caller get told to ask the store for more (`claim_next`, `backlog::Client`) — kept
//! as its own action rather than called here, since that's a network round trip. Deciding (`decide`) is
//! pure and returns actions; the caller applies each one itself (`apply`), or none of them (the dry driver). What's deliberately left out:
//! actually starting a session (implementer-session), the pause flag's real source (today it's
//! pushed from the store, backlog/06.2-operations.md's `pause`/`resume`), and `activeSessions` /
//! `concurrency` (no session pool exists yet to count).
pub mod driver;
pub mod guard;
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
    /// `item_id` was `queued` and takes `lane`; the caller starts its implementer session there.
    StartImplementer { item_id: String, lane: u32 },
    /// `item_id` was `queued` but is past its slice limit: it goes to `needs_human` and holds no lane
    /// (05-item-lifecycle.md), rather than starting an implementer.
    NeedsHuman { item_id: String },
    /// Nothing local is ready, and a lane is free: the caller asks the store (`claim_next`).
    AskForWork,
}

/// One project's tick decision, pure: nothing in `runs` or `pool` changes. A paused project does
/// nothing (section 7's "if paused: skip" skips the rest of the loop too). Otherwise every `queued`
/// item that can take a lane is given one (each pick is made on a scratch copy of the pool, so two
/// items never get the same lane), and only once none can is a free lane offered to `claim_next`.
/// The caller applies each action itself (`apply`), or, like the dry driver, none of them.
pub fn decide(project: &str, paused: bool, runs: &ProjectRuns, pool: &LanePool, cfg: &ProjectConfig) -> Vec<Action> {
    let mut actions = Vec::new();
    if paused {
        return actions;
    }
    let mut scratch = pool.clone();
    for (item_id, run) in runs.iter().filter(|(_, r)| r.state == ItemState::Queued) {
        let want = Want {
            project: project.to_string(),
            item: item_id.clone(),
            chore: false,
            last_lane: scratch.lane_of(project, item_id),
            fix_queued: false,
        };
        let Some(lane) = scratch.claim(&want) else { break };
        let mut after = *run;
        after.claimed(cfg);
        if after.state == ItemState::NeedsHuman {
            // The lane it briefly took is free again.
            scratch.release(
                lane,
                &Holder::Item {
                    project: project.to_string(),
                    item: item_id.clone(),
                },
            );
            actions.push(Action::NeedsHuman {
                item_id: item_id.clone(),
            });
        } else {
            actions.push(Action::StartImplementer {
                item_id: item_id.clone(),
                lane,
            });
        }
    }
    let ask_want = Want {
        project: project.to_string(),
        item: String::new(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    };
    if scratch.claim_lane(&ask_want).is_some() {
        actions.push(Action::AskForWork);
    }
    actions
}

/// Applies one decided action to the project's real `runs`, the shared `pool` and its `events`, as
/// if it were taken: the item moves to `implementing` and holds its lane (with the `lane_taken`
/// event), or moves to `needs_human`. Whether it was applied: a lane someone else took since the
/// decision is not taken over, and nothing changes. `AskForWork` changes nothing here; asking the
/// store is the caller's own step.
pub fn apply(
    project: &str,
    action: &Action,
    runs: &mut ProjectRuns,
    pool: &mut LanePool,
    cfg: &ProjectConfig,
    events: &mut EventLog,
) -> bool {
    match action {
        Action::StartImplementer { item_id, lane } => {
            let Some(run) = runs.get_mut(item_id) else { return false };
            let holder = Holder::Item {
                project: project.to_string(),
                item: item_id.clone(),
            };
            if run.state != ItemState::Queued || !pool.take(*lane, holder) {
                return false;
            }
            run.claimed(cfg);
            events.add_in(
                project,
                Event::LaneTaken(event::LaneTaken {
                    item: Some(item_id.clone()),
                    lane: Some(*lane),
                    ..Default::default()
                }),
            );
            true
        }
        Action::NeedsHuman { item_id } => {
            let Some(run) = runs.get_mut(item_id) else { return false };
            if run.state != ItemState::Queued {
                return false;
            }
            run.claimed(cfg);
            events.add_in(
                project,
                Event::ItemState(event::ItemState {
                    item: Some(item_id.clone()),
                    from: Some("queued".into()),
                    to: Some("needs_human".into()),
                    ..Default::default()
                }),
            );
            true
        }
        Action::AskForWork => true,
    }
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

    fn queued(ids: &[&str]) -> ProjectRuns {
        ids.iter().map(|id| (id.to_string(), ItemRun::new())).collect()
    }

    /// Decide, then apply every action, as a driver that takes everything would.
    fn tick_all(project: &str, runs: &mut ProjectRuns, pool: &mut LanePool, events: &mut EventLog) -> Vec<Action> {
        let actions = decide(project, false, runs, pool, &cfg());
        for a in &actions {
            apply(project, a, runs, pool, &cfg(), events);
        }
        actions
    }

    #[test]
    fn a_paused_project_does_nothing_even_with_a_queued_item_and_a_free_lane() {
        assert!(decide("proj", true, &queued(&["a"]), &pool(2), &cfg()).is_empty());
    }

    #[test]
    fn deciding_changes_nothing() {
        let runs = queued(&["a", "b"]);
        let pool = pool(2);
        let actions = decide("proj", false, &runs, &pool, &cfg());
        assert_eq!(actions.len(), 2, "two items, two lanes, nothing left to ask with");
        assert!(runs.values().all(|r| r.state == ItemState::Queued));
        assert_eq!(pool.held_lanes().count(), 0);
    }

    #[test]
    fn a_queued_item_takes_a_free_lane_and_moves_to_implementing_once_applied() {
        let mut runs = queued(&["a"]);
        let mut pool = pool(2);
        let mut events = EventLog::new();
        let actions = tick_all("proj", &mut runs, &mut pool, &mut events);
        assert_eq!(
            actions[0],
            Action::StartImplementer {
                item_id: "a".into(),
                lane: 1
            }
        );
        assert_eq!(runs["a"].state, ItemState::Implementing);
        assert_eq!(pool.lane_of("proj", "a"), Some(1));
        assert_eq!(events.recent()[0].kind(), "lane_taken");
    }

    #[test]
    fn every_queued_item_that_can_take_a_lane_does_in_one_tick() {
        let mut runs = queued(&["a", "b", "c"]);
        let mut pool = pool(2);
        let actions = tick_all("proj", &mut runs, &mut pool, &mut EventLog::new());
        // Only two lanes: two items start, the third is left queued with nothing offered for it
        // (both lanes are now busy, so there's nothing free to ask the store with either).
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|a| matches!(a, Action::StartImplementer { .. })));
        let still_queued = runs.values().filter(|r| r.state == ItemState::Queued).count();
        assert_eq!(still_queued, 1);
    }

    #[test]
    fn nothing_queued_and_a_free_lane_asks_the_store() {
        assert_eq!(
            decide("proj", false, &ProjectRuns::new(), &pool(2), &cfg()),
            vec![Action::AskForWork]
        );
    }

    #[test]
    fn every_lane_busy_asks_for_nothing() {
        let mut pool = pool(1);
        assert!(pool.take(
            1,
            Holder::Item {
                project: "proj".into(),
                item: "other".into(),
            }
        ));
        assert!(decide("proj", false, &ProjectRuns::new(), &pool, &cfg()).is_empty());
    }

    #[test]
    fn a_queued_item_past_the_slice_limit_needs_a_human_instead_of_implementing() {
        let mut run = ItemRun::new();
        run.slice = 10;
        let mut runs = ProjectRuns::from_iter([("a".to_string(), run)]);
        let mut pool = pool(1);
        let mut events = EventLog::new();
        // needs_human holds no lane, so the one lane is still free and the store is asked after it.
        let actions = tick_all("proj", &mut runs, &mut pool, &mut events);
        assert_eq!(
            actions,
            vec![Action::NeedsHuman { item_id: "a".into() }, Action::AskForWork]
        );
        assert_eq!(runs["a"].state, ItemState::NeedsHuman);
        assert_eq!(pool.lane_of("proj", "a"), None);
        let kinds: Vec<_> = events.recent().iter().map(|e| e.kind()).collect();
        assert_eq!(
            kinds,
            vec!["item_state"],
            "no lane_taken for an item that needs a human"
        );
    }

    #[test]
    fn a_lane_taken_since_the_decision_is_not_taken_over() {
        let mut runs = queued(&["a"]);
        let mut pool = pool(1);
        let actions = decide("proj", false, &runs, &pool, &cfg());
        assert!(pool.take(
            1,
            Holder::Item {
                project: "other".into(),
                item: "x".into()
            }
        ));
        assert!(!apply(
            "proj",
            &actions[0],
            &mut runs,
            &mut pool,
            &cfg(),
            &mut EventLog::new()
        ));
        assert_eq!(runs["a"].state, ItemState::Queued);
    }

    #[test]
    fn two_projects_on_one_shared_lane_pool_only_one_takes_it() {
        let mut pool = pool(1);
        let mut events = EventLog::new();
        let (mut a, mut b) = (queued(&["x"]), queued(&["y"]));
        let first = tick_all("a", &mut a, &mut pool, &mut events);
        let second = tick_all("b", &mut b, &mut pool, &mut events);
        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "the lane is held, so b neither starts nor asks");
        assert_eq!(b["y"].state, ItemState::Queued);
        assert_eq!(pool.lane_of("a", "x"), Some(1));
    }

    #[test]
    fn the_tick_payload_says_running_or_paused_and_its_exact_json() {
        let mut pool = pool(3);
        assert!(pool.take(
            2,
            Holder::Item {
                project: "proj".into(),
                item: "x".into(),
            }
        ));
        let mut events = EventLog::with_now(|| 0);
        events.add_in("proj", Event::Tick(tick_payload(false, &pool)));
        events.add_in("proj", Event::Tick(tick_payload(true, &pool)));
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
        assert_eq!(events.recent()[1].to_value()["status"], "paused");
    }
}
