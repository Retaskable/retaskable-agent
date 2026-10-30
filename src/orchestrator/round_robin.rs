//! Whose turn it is to start next, across every project on the computer (`roundRobin` in Retaskable.com's
//! `agent/src/pool.ts`). The lanes are the computer's, so projects take turns: one start per turn,
//! beginning with the project after the one served last, until no project can start. Every project's "do next"
//! items go first (the "rush" phase), and take no session slot: they don't wait for one (section 7). Only the
//! ordering and the fairness are here. `roundRobin` also runs the work and chore phases apart; chores aren't
//! built here, so every other start counts as "work".
use std::collections::BTreeSet;

use super::{Action, ProjectRuns, apply, decide};
use crate::config::ProjectConfig;
use crate::item_lifecycle::ItemState;
use crate::lane_pool::LanePool;
use crate::telemetry::EventLog;

/// A project taking part in one pass: what `decide` needs of it.
pub struct Candidate<'a> {
    pub id: &'a str,
    pub paused: bool,
    /// The project's runs; starts decided in the pass are applied to this copy (not the project's own), so
    /// the same item is never started twice.
    pub runs: ProjectRuns,
    pub cfg: &'a ProjectConfig,
    /// The items a person marked "do next" (the store's `doNext`).
    pub rush: BTreeSet<String>,
}

/// What each project would do this pass (`None` for a project not taking part), and who was served last.
/// Nothing real changes: starts are applied to a copy of `pool` only, so a lane one project is given is
/// already gone when the next one is asked. `last_served` is an index into `candidates`. `slots` is how
/// many more sessions may start (`concurrency` less `activeSessions`): each start takes one, and with none
/// left nothing more starts and nobody asks the store. A "do next" item starts first, while a lane is free,
/// whatever the slots, and takes none.
pub fn plan(
    pool: &LanePool,
    last_served: Option<usize>,
    slots: u32,
    candidates: &mut [Option<Candidate>],
) -> (Vec<Vec<Action>>, Option<usize>) {
    let n = candidates.len();
    let mut scratch = pool.clone();
    let mut log = EventLog::new();
    let mut actions = vec![Vec::new(); n];
    let mut last = last_served;
    let mut slots = slots;
    for (i, c) in candidates.iter_mut().enumerate() {
        let Some(c) = c else { continue };
        while let Some(start) = next_start(c, &mut scratch, &mut log, true) {
            actions[i].push(start);
        }
    }
    'turns: while slots > 0 {
        let first = last.map_or(0, |l| l + 1);
        for i in (0..n).map(|k| (first + k) % n) {
            let Some(c) = candidates[i].as_mut() else { continue };
            if let Some(start) = next_start(c, &mut scratch, &mut log, false) {
                actions[i].push(start);
                last = Some(i);
                slots -= 1;
                continue 'turns;
            }
        }
        break;
    }
    // What is left once nobody can start: a project with a lane still free asks the store.
    for (i, c) in candidates.iter().enumerate() {
        if let Some(c) = c
            && slots > 0
            && decide(c.id, c.paused, &c.runs, &scratch, c.cfg).contains(&Action::AskForWork)
        {
            actions[i].push(Action::AskForWork);
        }
    }
    (actions, last)
}

/// The project's next start (with `rush`, of a "do next" item only), applied to the copies. An item over its
/// slice limit goes to `needs_human` on the way: that holds no lane and is no turn.
fn next_start(c: &mut Candidate, pool: &mut LanePool, log: &mut EventLog, rush: bool) -> Option<Action> {
    let decided = if rush {
        let only: ProjectRuns = (c.runs.iter())
            .filter(|(id, run)| run.state != ItemState::Queued || c.rush.contains(*id))
            .map(|(id, run)| (id.clone(), *run))
            .collect();
        decide(c.id, c.paused, &only, pool, c.cfg)
    } else {
        decide(c.id, c.paused, &c.runs, pool, c.cfg)
    };
    for action in decided {
        match action {
            Action::StartImplementer { .. } => {
                if apply(c.id, &action, &mut c.runs, pool, c.cfg, log) {
                    return Some(action);
                }
            }
            Action::NeedsHuman { .. } => {
                apply(c.id, &action, &mut c.runs, pool, c.cfg, log);
            }
            Action::AskForWork => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item_lifecycle::ItemRun;
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

    fn candidate<'a>(id: &'a str, runs: ProjectRuns, cfg: &'a ProjectConfig) -> Candidate<'a> {
        Candidate {
            id,
            paused: false,
            runs,
            cfg,
            rush: BTreeSet::new(),
        }
    }

    #[test]
    fn a_do_next_item_starts_in_the_rush_phase_even_with_no_slots_left() {
        let cfg = cfg();
        let mut c = candidate("a", queued(&["x"]), &cfg);
        c.rush.insert("x".to_string());
        let mut candidates = [Some(c)];
        let (actions, last) = plan(&pool(1), None, 0, &mut candidates);
        assert_eq!(
            actions[0],
            vec![Action::StartImplementer {
                item_id: "x".into(),
                lane: 1
            }]
        );
        assert_eq!(
            last, None,
            "the rush phase isn't a turn, so it doesn't change who was served last"
        );
    }

    #[test]
    fn only_do_next_items_run_in_the_rush_phase_a_plain_queued_item_waits_its_turn() {
        let cfg = cfg();
        let mut c = candidate("a", queued(&["x", "y"]), &cfg);
        c.rush.insert("x".to_string());
        let mut candidates = [Some(c)];
        let (actions, _) = plan(&pool(2), None, 0, &mut candidates);
        assert_eq!(
            actions[0],
            vec![Action::StartImplementer {
                item_id: "x".into(),
                lane: 1
            }],
            "y isn't a do-next item, and there are no turn slots left for it"
        );
    }

    #[test]
    fn turns_go_round_robin_starting_after_whoever_was_served_last() {
        let cfg = cfg();
        let mut candidates = [
            Some(candidate("a", queued(&["a1"]), &cfg)),
            Some(candidate("b", queued(&["b1"]), &cfg)),
        ];
        let (actions, last) = plan(&pool(2), Some(0), 1, &mut candidates);
        assert_eq!(actions[0], Vec::new(), "a was served last, so b goes this turn");
        assert_eq!(
            actions[1],
            vec![Action::StartImplementer {
                item_id: "b1".into(),
                lane: 1
            }]
        );
        assert_eq!(last, Some(1));
    }

    #[test]
    fn last_served_wraps_from_the_last_candidate_back_to_the_first() {
        let cfg = cfg();
        let mut candidates = [
            Some(candidate("a", queued(&["a1"]), &cfg)),
            Some(candidate("b", queued(&["b1"]), &cfg)),
        ];
        let (actions, last) = plan(&pool(2), Some(1), 1, &mut candidates);
        assert_eq!(
            actions[0],
            vec![Action::StartImplementer {
                item_id: "a1".into(),
                lane: 1
            }],
            "b was served last, so a goes next, wrapping back to the front"
        );
        assert_eq!(actions[1], Vec::new());
        assert_eq!(last, Some(0));
    }

    #[test]
    fn an_item_past_the_slice_limit_goes_to_needs_human_and_is_not_a_turn() {
        let cfg = cfg();
        let mut over_limit = ItemRun::new();
        over_limit.slice = 10;
        let mut candidates = [
            Some(candidate(
                "a",
                ProjectRuns::from_iter([("a1".to_string(), over_limit)]),
                &cfg,
            )),
            Some(candidate("b", queued(&["b1"]), &cfg)),
        ];
        let (actions, last) = plan(&pool(2), None, 1, &mut candidates);
        // needs_human is applied on the way (not returned as an action to start), holds no lane,
        // and spends no turn: the one slot still reaches b.
        assert_eq!(actions[0], Vec::new());
        assert_eq!(candidates[0].as_ref().unwrap().runs["a1"].state, ItemState::NeedsHuman);
        assert_eq!(
            actions[1],
            vec![Action::StartImplementer {
                item_id: "b1".into(),
                lane: 1
            }]
        );
        assert_eq!(last, Some(1));
    }

    #[test]
    fn a_paused_project_takes_no_turn_and_is_never_asked_for_work() {
        let cfg = cfg();
        let mut c = candidate("a", queued(&["x"]), &cfg);
        c.paused = true;
        let mut candidates = [Some(c)];
        let (actions, last) = plan(&pool(2), None, 5, &mut candidates);
        assert_eq!(actions[0], Vec::new());
        assert_eq!(last, None);
    }

    #[test]
    fn a_free_lane_with_nothing_left_to_start_asks_the_store() {
        let cfg = cfg();
        let candidate = candidate("a", ProjectRuns::new(), &cfg);
        let mut candidates = [Some(candidate)];
        let (actions, last) = plan(&pool(1), None, 1, &mut candidates);
        assert_eq!(actions[0], vec![Action::AskForWork]);
        assert_eq!(last, None, "asking the store isn't a turn either");
    }

    #[test]
    fn a_project_not_taking_part_this_pass_is_skipped_but_others_still_take_their_turn() {
        let cfg = cfg();
        let mut candidates = [None, Some(candidate("b", queued(&["b1"]), &cfg))];
        let (actions, last) = plan(&pool(1), None, 1, &mut candidates);
        assert_eq!(actions[0], Vec::new());
        assert_eq!(
            actions[1],
            vec![Action::StartImplementer {
                item_id: "b1".into(),
                lane: 1
            }]
        );
        assert_eq!(last, Some(1));
    }
}
