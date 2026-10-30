//! Whose turn it is to start next, across every project on the computer (`roundRobin` in Retaskable.com's
//! `agent/src/pool.ts`). The lanes are the computer's, so projects take turns: one start per turn,
//! beginning with the project after the one served last, until no project can start. Only the ordering
//! and the fairness are here. `roundRobin` also puts every project's "rush" starts first and then runs the
//! work and chore phases apart; that classification belongs to the start-rules slice, so for now every
//! start counts as "work".
use super::{Action, ProjectRuns, apply, decide};
use crate::config::ProjectConfig;
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
}

/// What each project would do this pass (`None` for a project not taking part), and who was served last.
/// Nothing real changes: starts are applied to a copy of `pool` only, so a lane one project is given is
/// already gone when the next one is asked. `last_served` is an index into `candidates`.
pub fn plan(
    pool: &LanePool,
    last_served: Option<usize>,
    candidates: &mut [Option<Candidate>],
) -> (Vec<Vec<Action>>, Option<usize>) {
    let n = candidates.len();
    let mut scratch = pool.clone();
    let mut log = EventLog::new();
    let mut actions = vec![Vec::new(); n];
    let mut last = last_served;
    'turns: loop {
        let first = last.map_or(0, |l| l + 1);
        for i in (0..n).map(|k| (first + k) % n) {
            let Some(c) = candidates[i].as_mut() else { continue };
            if let Some(start) = next_start(c, &mut scratch, &mut log) {
                actions[i].push(start);
                last = Some(i);
                continue 'turns;
            }
        }
        break;
    }
    // What is left once nobody can start: a project with a lane still free asks the store.
    for (i, c) in candidates.iter().enumerate() {
        if let Some(c) = c
            && decide(c.id, c.paused, &c.runs, &scratch, c.cfg).contains(&Action::AskForWork)
        {
            actions[i].push(Action::AskForWork);
        }
    }
    (actions, last)
}

/// The project's next start, applied to the copies. An item over its slice limit goes to `needs_human`
/// on the way: that holds no lane and is no turn.
fn next_start(c: &mut Candidate, pool: &mut LanePool, log: &mut EventLog) -> Option<Action> {
    for action in decide(c.id, c.paused, &c.runs, pool, c.cfg) {
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
