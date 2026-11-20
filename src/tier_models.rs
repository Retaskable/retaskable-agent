//! The three tiers and the model each runs on (docs/specs/engines/03-choosing-the-engine.md section 3.4,
//! docs/specs/agent/07.1-model-tiers.md). The store names a tier only; this computer has one company (its `engine`)
//! and one model per tier (`models.smart`, `models.fast`, `models.cheap`), and this module turns the one into the other.
//!
//! - Smart is the strongest: plans, reviews, escalations. The store's `opus`.
//! - Fast is the normal building model. The store's `sonnet`.
//! - Cheap is for quick looks and judges: scouts, classifiers, release notes. `haiku`.
use std::sync::OnceLock;

use crate::config::{CursorModels, Engine};
use crate::engines::{EngineId, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Smart,
    Fast,
    Cheap,
}

impl Tier {
    /// The tier a Claude Code model name stands for (`opus`, `sonnet`, `haiku`: what the store and the ladder name).
    /// `local` and anything unknown are Fast, the normal building model.
    pub fn of_name(name: &str) -> Tier {
        match name {
            "opus" => Tier::Smart,
            "haiku" => Tier::Cheap,
            _ => Tier::Fast,
        }
    }

    /// The tier a kind of work runs on (section 3.4's table): the planner, reviewer, drafter and triager are Smart; the
    /// implementer is Fast; scouts, the writer and the assistant are Cheap.
    pub fn of_role(role: Role) -> Tier {
        match role {
            Role::Planner | Role::Reviewer | Role::Drafter | Role::Triager => Tier::Smart,
            Role::Implementer => Tier::Fast,
            Role::Scout | Role::Writer | Role::Assistant => Tier::Cheap,
        }
    }
}

/// A company's model for each tier when none is set. Claude Code: opus, sonnet, haiku. Cursor: `auto`, its own pick.
/// Codex and pi (and the local model, which has the one model named under Local model): empty, which means the program's own
/// default, so no model flag is sent.
pub fn defaults(engine: Engine) -> [&'static str; 3] {
    match engine {
        Engine::Claude => ["opus", "sonnet", "haiku"],
        Engine::Cursor => ["auto", "auto", "auto"],
        Engine::Pi | Engine::Codex | Engine::Local => ["", "", ""],
    }
}

/// The model `tier` runs on under `engine`: the set name, else the company's default ("" is the program's own default).
pub fn model_for(engine: Engine, models: &CursorModels, tier: Tier) -> String {
    let (set, [smart, fast, cheap]) = (
        match tier {
            Tier::Smart => &models.top,
            Tier::Fast => &models.implementer,
            Tier::Cheap => &models.scout,
        },
        defaults(engine),
    );
    let default = match tier {
        Tier::Smart => smart,
        Tier::Fast => fast,
        Tier::Cheap => cheap,
    };
    set.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
        .to_string()
}

static CHOSEN: OnceLock<(Engine, CursorModels)> = OnceLock::new();

/// Hands over this computer's company and tier models, once, at start.
pub fn set_global(engine: Engine, models: CursorModels) {
    let _ = CHOSEN.set((engine, models));
}

/// The model a session on `program` runs when it was started on the model named `name` (`opus`, `sonnet`, `haiku`):
/// the chosen company's model for that name's tier. `None` when the chosen company isn't `program`'s (a helper that is
/// always Claude Code keeps its own name), when nothing was handed over, or when the company's default is the program's
/// own (an empty name): the caller keeps what it had, or sends no model flag.
pub fn session_model(program: EngineId, name: &str) -> Option<String> {
    let (engine, models) = CHOSEN.get()?;
    let chosen = crate::engines::program_of(*engine);
    if chosen != program || name == "local" {
        return None;
    }
    Some(model_for(*engine, models, Tier::of_name(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(top: Option<&str>, implementer: Option<&str>, scout: Option<&str>) -> CursorModels {
        let s = |v: Option<&str>| v.map(String::from);
        CursorModels {
            top: s(top),
            implementer: s(implementer),
            scout: s(scout),
        }
    }

    #[test]
    fn the_stores_names_are_the_three_tiers() {
        assert_eq!(Tier::of_name("opus"), Tier::Smart);
        assert_eq!(Tier::of_name("sonnet"), Tier::Fast);
        assert_eq!(Tier::of_name("haiku"), Tier::Cheap);
        assert_eq!(Tier::of_name("local"), Tier::Fast);
    }

    #[test]
    fn each_kind_of_work_runs_on_its_tier() {
        for (role, tier) in [
            (Role::Planner, Tier::Smart),
            (Role::Reviewer, Tier::Smart),
            (Role::Drafter, Tier::Smart),
            (Role::Triager, Tier::Smart),
            (Role::Implementer, Tier::Fast),
            (Role::Scout, Tier::Cheap),
            (Role::Writer, Tier::Cheap),
            (Role::Assistant, Tier::Cheap),
        ] {
            assert_eq!(Tier::of_role(role), tier, "{role:?}");
        }
    }

    #[test]
    fn claude_defaults_to_opus_sonnet_haiku() {
        let none = CursorModels::default();
        let got: Vec<_> = [Tier::Smart, Tier::Fast, Tier::Cheap]
            .into_iter()
            .map(|t| model_for(Engine::Claude, &none, t))
            .collect();
        assert_eq!(got, ["opus", "sonnet", "haiku"]);
    }

    #[test]
    fn cursor_defaults_to_auto_and_the_others_to_the_programs_own() {
        let none = CursorModels::default();
        assert_eq!(model_for(Engine::Cursor, &none, Tier::Smart), "auto");
        for e in [Engine::Codex, Engine::Pi, Engine::Local] {
            assert_eq!(model_for(e, &none, Tier::Fast), "", "{e:?}");
        }
    }

    #[test]
    fn a_set_model_wins_per_tier_and_a_blank_one_is_the_default() {
        let m = set(Some("claude-opus-4-1"), Some("  "), None);
        assert_eq!(model_for(Engine::Claude, &m, Tier::Smart), "claude-opus-4-1");
        assert_eq!(model_for(Engine::Claude, &m, Tier::Fast), "sonnet");
        assert_eq!(model_for(Engine::Claude, &m, Tier::Cheap), "haiku");
        let m = set(Some("gpt-5"), None, Some("gpt-5-mini"));
        assert_eq!(model_for(Engine::Codex, &m, Tier::Smart), "gpt-5");
        assert_eq!(model_for(Engine::Codex, &m, Tier::Cheap), "gpt-5-mini");
    }
}
