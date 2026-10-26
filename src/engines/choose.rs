//! Which program runs a session, looked up when it starts (docs/specs/engines/03-choosing-the-engine.md
//! section 3.1), following `agent/src/engines/choose.ts` in Retaskable.com: the computer's own
//! `engineChoice`, a project's older `engines.default`/`roles` while the computer hasn't saved its own
//! choice yet (`legacyChoice`), and `engineChoice.lanes`' per-lane override for the implementer's work.
use serde_json::Value;

use crate::config::{Engine, EngineChoiceConfig, LaneEngine, RoleEngines};

/// `Role` in `shared/types.ts`: the kind of work a session does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Planner,
    Implementer,
    Scout,
    Reviewer,
    Drafter,
    Assistant,
    Triager,
    Writer,
}

/// This role's group in `engineChoice.roles` (section 3.1's table): `scout` and `reviewer` share
/// `review`; `assistant` and `triager` share `chat`.
fn role_override(roles: &RoleEngines, role: Role) -> Option<Engine> {
    match role {
        Role::Planner => roles.planner,
        Role::Implementer => roles.implementer,
        Role::Scout | Role::Reviewer => roles.review,
        Role::Drafter => roles.drafter,
        Role::Assistant | Role::Triager => roles.chat,
        Role::Writer => roles.writer,
    }
}

/// Whether `engineChoice` has nothing in it at all — no default, no role override, no lanes — the
/// computer hasn't saved its own choice yet, so a project's older `engines.default`/`roles` still
/// counts (`legacyChoice`, below).
fn is_unset(cfg: &EngineChoiceConfig) -> bool {
    *cfg == EngineChoiceConfig::default()
}

/// `legacyChoice`: a project's older `engines.default`/`roles`, read the same lenient way as the
/// computer's own `engineChoice` (an unknown engine, or a malformed `roles`, reads as unset rather
/// than failing). Per-lane engines aren't part of this older shape, so a stray `lanes` in it is
/// never looked at: `engine_for` only reads lanes off the computer's own `engineChoice`.
fn legacy_choice(project_engines: Option<&Value>) -> EngineChoiceConfig {
    project_engines
        .and_then(|v| serde_json::from_value::<EngineChoiceConfig>(v.clone()).ok())
        .unwrap_or_default()
}

/// `laneEngine`: lane `lane`'s own engine (1-based, lane 1 first) from `engineChoice.lanes`,
/// `None` for a `default` entry, a lane past the end of the list, or no list at all.
pub fn lane_engine(cfg: &EngineChoiceConfig, lane: u32) -> Option<Engine> {
    let index = lane.checked_sub(1)?;
    match cfg.lanes.get(index as usize)? {
        LaneEngine::Claude => Some(Engine::Claude),
        LaneEngine::Cursor => Some(Engine::Cursor),
        LaneEngine::Default => None,
    }
}

/// `hasLaneEngines`: whether this computer set any per-lane engine at all.
pub fn has_lane_engines(cfg: &EngineChoiceConfig) -> bool {
    !cfg.lanes.is_empty()
}

/// `engineFor(cfg, role, lane)`: which program runs a session of this `role`. Only the
/// implementer's work follows its lane (`lane`, the computer's lane number, 1-based); every other
/// role follows its own group's engine, or the default. Missing everywhere is Claude Code.
pub fn engine_for(cfg: &EngineChoiceConfig, project_engines: Option<&Value>, role: Role, lane: Option<u32>) -> Engine {
    let active = if is_unset(cfg) {
        legacy_choice(project_engines)
    } else {
        cfg.clone()
    };

    if role == Role::Implementer
        && let Some(lane) = lane
        && let Some(engine) = lane_engine(&active, lane)
    {
        return engine;
    }

    role_override(&active.roles, role)
        .or(active.default)
        .unwrap_or(Engine::Claude)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_default(engine: Engine) -> EngineChoiceConfig {
        EngineChoiceConfig {
            default: Some(engine),
            ..EngineChoiceConfig::default()
        }
    }

    #[test]
    fn nothing_set_anywhere_is_claude_for_every_role() {
        let cfg = EngineChoiceConfig::default();
        for role in [
            Role::Planner,
            Role::Implementer,
            Role::Scout,
            Role::Reviewer,
            Role::Drafter,
            Role::Assistant,
            Role::Triager,
            Role::Writer,
        ] {
            assert_eq!(engine_for(&cfg, None, role, None), Engine::Claude);
        }
    }

    #[test]
    fn a_default_applies_to_every_role_with_no_override() {
        let cfg = with_default(Engine::Cursor);
        assert_eq!(engine_for(&cfg, None, Role::Planner, None), Engine::Cursor);
        assert_eq!(engine_for(&cfg, None, Role::Writer, None), Engine::Cursor);
    }

    #[test]
    fn a_role_override_wins_over_the_default() {
        let mut cfg = with_default(Engine::Cursor);
        cfg.roles.implementer = Some(Engine::Local);
        assert_eq!(engine_for(&cfg, None, Role::Implementer, None), Engine::Local);
        // Everything else still follows the default.
        assert_eq!(engine_for(&cfg, None, Role::Planner, None), Engine::Cursor);
    }

    #[test]
    fn scout_and_reviewer_share_the_review_group() {
        let mut cfg = EngineChoiceConfig::default();
        cfg.roles.review = Some(Engine::Cursor);
        assert_eq!(engine_for(&cfg, None, Role::Scout, None), Engine::Cursor);
        assert_eq!(engine_for(&cfg, None, Role::Reviewer, None), Engine::Cursor);
    }

    #[test]
    fn assistant_and_triager_share_the_chat_group() {
        let mut cfg = EngineChoiceConfig::default();
        cfg.roles.chat = Some(Engine::Pi);
        assert_eq!(engine_for(&cfg, None, Role::Assistant, None), Engine::Pi);
        assert_eq!(engine_for(&cfg, None, Role::Triager, None), Engine::Pi);
    }

    #[test]
    fn an_unset_engine_choice_falls_back_to_the_projects_legacy_engines() {
        let cfg = EngineChoiceConfig::default();
        let legacy = serde_json::json!({ "default": "cursor", "roles": { "implementer": "local" } });
        assert_eq!(engine_for(&cfg, Some(&legacy), Role::Writer, None), Engine::Cursor);
        assert_eq!(engine_for(&cfg, Some(&legacy), Role::Implementer, None), Engine::Local);
    }

    #[test]
    fn once_the_computer_has_any_choice_of_its_own_legacy_is_ignored_entirely() {
        // Even a role the computer didn't override falls back to Claude, not the project's legacy
        // default — "until the computer saves its own choice" means the whole legacy shape, not
        // field by field.
        let mut cfg = EngineChoiceConfig::default();
        cfg.roles.implementer = Some(Engine::Cursor);
        let legacy = serde_json::json!({ "default": "pi" });
        assert_eq!(engine_for(&cfg, Some(&legacy), Role::Writer, None), Engine::Claude);
        assert_eq!(engine_for(&cfg, Some(&legacy), Role::Implementer, None), Engine::Cursor);
    }

    #[test]
    fn a_malformed_legacy_value_reads_as_nothing_rather_than_failing() {
        let cfg = EngineChoiceConfig::default();
        let legacy = serde_json::json!("not an object");
        assert_eq!(engine_for(&cfg, Some(&legacy), Role::Planner, None), Engine::Claude);
    }

    #[test]
    fn a_lane_override_wins_for_the_implementer_only() {
        let mut cfg = with_default(Engine::Claude);
        cfg.lanes = vec![LaneEngine::Claude, LaneEngine::Cursor];
        assert_eq!(engine_for(&cfg, None, Role::Implementer, Some(1)), Engine::Claude);
        assert_eq!(engine_for(&cfg, None, Role::Implementer, Some(2)), Engine::Cursor);
        // Any other role ignores the lane list entirely, even when one is given.
        assert_eq!(engine_for(&cfg, None, Role::Scout, Some(2)), Engine::Claude);
    }

    #[test]
    fn a_lane_past_the_end_of_the_list_falls_back_to_the_roles_own_engine() {
        let mut cfg = with_default(Engine::Cursor);
        cfg.lanes = vec![LaneEngine::Claude];
        assert_eq!(engine_for(&cfg, None, Role::Implementer, Some(2)), Engine::Cursor);
        assert_eq!(engine_for(&cfg, None, Role::Implementer, None), Engine::Cursor);
    }

    #[test]
    fn a_default_lane_entry_follows_the_implementers_own_engine() {
        let mut cfg = with_default(Engine::Claude);
        cfg.roles.implementer = Some(Engine::Cursor);
        cfg.lanes = vec![LaneEngine::Default, LaneEngine::Claude];
        assert_eq!(engine_for(&cfg, None, Role::Implementer, Some(1)), Engine::Cursor);
        assert_eq!(engine_for(&cfg, None, Role::Implementer, Some(2)), Engine::Claude);
    }

    #[test]
    fn lane_engine_and_has_lane_engines_read_the_list_directly() {
        let mut cfg = EngineChoiceConfig::default();
        assert!(!has_lane_engines(&cfg));
        assert_eq!(lane_engine(&cfg, 1), None);
        cfg.lanes = vec![LaneEngine::Cursor, LaneEngine::Default];
        assert!(has_lane_engines(&cfg));
        assert_eq!(lane_engine(&cfg, 1), Some(Engine::Cursor));
        assert_eq!(lane_engine(&cfg, 2), None);
        assert_eq!(lane_engine(&cfg, 3), None);
    }
}
