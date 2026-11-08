//! Which engine may start new work now (docs/specs/engines/03-choosing-the-engine.md sections 3.2 and 3.5):
//! `enginesOut`, `fallOver`/`engineNow`, `usesClaude`/`usesCursor` and `laneMayStart`, all plain code.
use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::Value;

use super::EngineId;
use super::choose::{GROUP_ROLES, Role, engine_for, program_of};
use crate::config::EngineChoiceConfig;
use crate::orchestrator::fresh_start::EngineLimits;

/// `enginesOut`: Cursor, pi and Codex while their own limit hasn't passed; Claude Code while it is limited
/// or signed out on this computer (`claude_signed_out_until` hasn't passed).
pub fn engines_out(
    limits: &EngineLimits,
    claude_signed_out_until: Option<Instant>,
    now: Instant,
) -> BTreeSet<EngineId> {
    let mut out: BTreeSet<EngineId> = [EngineId::Claude, EngineId::Cursor, EngineId::Pi, EngineId::Codex]
        .into_iter()
        .filter(|e| limits.is_limited(*e, now))
        .collect();
    if claude_signed_out_until.is_some_and(|until| until > now) {
        out.insert(EngineId::Claude);
    }
    out
}

/// `fallOver` + `engineNow`: `chosen` unless it is out, then the first other engine this computer already
/// uses (`used_here`) that isn't out. A pi choice never falls over; with nothing to take over it stays `chosen`.
pub fn engine_now(chosen: EngineId, used_here: &[EngineId], out: &BTreeSet<EngineId>) -> EngineId {
    if chosen == EngineId::Pi || !out.contains(&chosen) {
        return chosen;
    }
    used_here
        .iter()
        .copied()
        .find(|e| *e != chosen && !out.contains(e))
        .unwrap_or(chosen)
}

/// `usesClaude`/`usesCursor`: every engine any role group or any lane `1..=lanes` runs on, first seen first.
pub fn engines_used_here(choice: &EngineChoiceConfig, project_engines: Option<&Value>, lanes: u32) -> Vec<EngineId> {
    let mut used = Vec::new();
    let picks = GROUP_ROLES
        .iter()
        .map(|(_, role)| engine_for(choice, project_engines, *role, None))
        .chain((1..=lanes).map(|k| engine_for(choice, project_engines, Role::Implementer, Some(k))));
    for engine in picks.map(program_of) {
        if !used.contains(&engine) {
            used.push(engine);
        }
    }
    used
}

/// Whether a new session may start on `engine`: it isn't out.
pub fn may_start(engine: EngineId, out: &BTreeSet<EngineId>) -> bool {
    !out.contains(&engine)
}

/// `laneMayStart`: lane `lane`'s implementer engine, after `engine_now`, isn't out.
pub fn lane_may_start(
    choice: &EngineChoiceConfig,
    project_engines: Option<&Value>,
    lane: u32,
    used_here: &[EngineId],
    out: &BTreeSet<EngineId>,
) -> bool {
    let chosen = program_of(engine_for(choice, project_engines, Role::Implementer, Some(lane)));
    may_start(engine_now(chosen, used_here, out), out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Engine, LaneEngine};
    use EngineId::{Claude, Codex, Cursor, Pi};
    use std::time::Duration;

    fn limited(engine: EngineId, now: Instant) -> EngineLimits {
        let mut l = EngineLimits::default();
        l.hit(engine, now + Duration::from_secs(60));
        l
    }

    #[test]
    fn each_engine_is_out_until_its_limit_passes() {
        let now = Instant::now();
        for e in [Claude, Cursor, Pi, Codex] {
            let l = limited(e, now);
            assert_eq!(engines_out(&l, None, now), BTreeSet::from([e]));
            assert!(engines_out(&l, None, now + Duration::from_secs(61)).is_empty());
        }
    }

    #[test]
    fn claude_is_out_while_signed_out_and_cursor_is_not() {
        let now = Instant::now();
        let until = Some(now + Duration::from_secs(5));
        assert_eq!(
            engines_out(&EngineLimits::default(), until, now),
            BTreeSet::from([Claude])
        );
        assert!(engines_out(&EngineLimits::default(), until, now + Duration::from_secs(5)).is_empty());
        assert!(!engines_out(&limited(Claude, now), until, now).contains(&Cursor));
    }

    #[test]
    fn a_chosen_engine_falls_over_only_to_one_used_here_and_not_out() {
        let c = BTreeSet::from([Cursor]);
        let l = BTreeSet::from([Claude]);
        assert_eq!(engine_now(Cursor, &[Cursor, Claude], &c), Claude);
        assert_eq!(engine_now(Claude, &[Cursor, Claude], &l), Cursor);
        assert_eq!(engine_now(Cursor, &[Cursor], &c), Cursor, "Claude isn't used here");
        assert_eq!(
            engine_now(Cursor, &[Cursor, Claude], &BTreeSet::from([Cursor, Claude])),
            Cursor
        );
        assert_eq!(engine_now(Claude, &[Claude, Cursor], &BTreeSet::new()), Claude);
        assert_eq!(engine_now(Pi, &[Pi, Claude], &BTreeSet::from([Pi])), Pi);
    }

    #[test]
    fn used_here_lists_every_role_and_lane_engine() {
        let cursor_only = EngineChoiceConfig {
            default: Some(Engine::Cursor),
            ..EngineChoiceConfig::default()
        };
        assert_eq!(engines_used_here(&cursor_only, None, 2), vec![Cursor]);
        let mixed = EngineChoiceConfig {
            default: Some(Engine::Cursor),
            lanes: vec![LaneEngine::Claude],
            ..EngineChoiceConfig::default()
        };
        assert_eq!(engines_used_here(&mixed, None, 2), vec![Cursor, Claude]);
        assert_eq!(engines_used_here(&mixed, None, 0), vec![Cursor]);
    }

    #[test]
    fn a_lane_may_start_on_the_engine_that_takes_over() {
        let out = BTreeSet::from([Claude]);
        let mixed = EngineChoiceConfig {
            default: Some(Engine::Cursor),
            lanes: vec![LaneEngine::Claude, LaneEngine::Cursor],
            ..EngineChoiceConfig::default()
        };
        let used = engines_used_here(&mixed, None, 2);
        assert!(
            lane_may_start(&mixed, None, 2, &used, &out),
            "a Cursor lane while Claude is out"
        );
        assert!(
            lane_may_start(&mixed, None, 1, &used, &out),
            "Claude's lane falls over to Cursor"
        );
        let claude_only = EngineChoiceConfig::default();
        let used = engines_used_here(&claude_only, None, 2);
        assert!(
            !lane_may_start(&claude_only, None, 1, &used, &out),
            "nothing else is used here"
        );
        assert!(may_start(Cursor, &out) && !may_start(Claude, &out));
    }
}
