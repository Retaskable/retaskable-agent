//! Turning a chosen engine into what a session needs to start (docs/specs/engines/
//! 03-choosing-the-engine.md sections 3.1 and 3.4): the program, its binary, and for Cursor the
//! role's model name. One shared place, so no starter has to hard-code Claude.
use std::path::PathBuf;

use crate::config::{CursorModels, Engine};
use crate::engines::engine::CursorOptions;
use crate::engines::resolve::{
    resolve_claude_on_path, resolve_codex_on_path, resolve_cursor_on_path, resolve_pi_on_path,
};
use crate::engines::{EngineId, Role, program_of};
use crate::env::Env;
use crate::orchestrator::session::exists;

/// What a session needs to start on its chosen engine.
#[derive(Debug, Clone)]
pub struct Placement {
    pub engine: EngineId,
    pub bin: PathBuf,
    /// Only on Cursor: the role's model name.
    pub cursor: Option<CursorOptions>,
    /// The engine was set to `local`: the session takes the local model while it's available (and the
    /// local ladder is on), else runs on the role's tier.
    pub local: bool,
}

/// `cursorModelFor(cfg, role)` (section 3.4): `top` for the planner, reviewer, drafter and
/// triager; `implementer` for the implementer; `scout` for the scout, writer and assistant.
/// A missing, blank or invalid name reads as `auto`.
pub fn cursor_model_for(models: &CursorModels, role: Role) -> String {
    let name = match role {
        Role::Planner | Role::Reviewer | Role::Drafter | Role::Triager => &models.top,
        Role::Implementer => &models.implementer,
        Role::Scout | Role::Writer | Role::Assistant => &models.scout,
    };
    name.as_deref()
        .map(str::trim)
        .filter(|n| crate::config::valid_cursor_model_name(n))
        .unwrap_or("auto")
        .to_string()
}

/// Resolves `engine`'s binary and checks it exists. `local` places as Claude Code with `local` set: the
/// caller then gives the session the `LocalTarget` and the local slot while the local model is available
/// (agent/07.2), and otherwise runs it on the role's tier (section 3.1). The other engines never take the slot.
pub fn place(engine: Engine, role: Role, env: &Env, cursor_models: &CursorModels) -> Result<Placement, String> {
    let program = program_of(engine);
    let (name, bin) = match program {
        EngineId::Claude => ("claude", resolve_claude_on_path(env).map_err(|e| format!("{e:#}"))?),
        EngineId::Cursor => ("cursor-agent", resolve_cursor_on_path(env)),
        EngineId::Pi => ("pi", resolve_pi_on_path(env)),
        EngineId::Codex => ("codex", resolve_codex_on_path(env)),
    };
    if !exists(&bin, env) {
        return Err(format!("the {name} binary wasn't found ({})", bin.display()));
    }
    let cursor = (program == EngineId::Cursor).then(|| CursorOptions {
        model: Some(cursor_model_for(cursor_models, role)),
        ..CursorOptions::default()
    });
    Ok(Placement {
        engine: program,
        bin,
        cursor,
        local: engine == Engine::Local,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn models(top: Option<&str>, implementer: Option<&str>, scout: Option<&str>) -> CursorModels {
        let s = |v: Option<&str>| v.map(String::from);
        CursorModels {
            top: s(top),
            implementer: s(implementer),
            scout: s(scout),
        }
    }

    #[test]
    fn every_role_maps_to_its_section_3_4_name() {
        let m = models(Some("gpt-5"), Some("sonnet-4.5"), Some("fast"));
        for (role, want) in [
            (Role::Planner, "gpt-5"),
            (Role::Reviewer, "gpt-5"),
            (Role::Drafter, "gpt-5"),
            (Role::Triager, "gpt-5"),
            (Role::Implementer, "sonnet-4.5"),
            (Role::Scout, "fast"),
            (Role::Writer, "fast"),
            (Role::Assistant, "fast"),
        ] {
            assert_eq!(cursor_model_for(&m, role), want, "{role:?}");
        }
    }

    #[test]
    fn a_missing_blank_or_flag_name_is_auto() {
        let m = models(None, Some("  "), Some("--force"));
        assert_eq!(cursor_model_for(&m, Role::Planner), "auto");
        assert_eq!(cursor_model_for(&m, Role::Implementer), "auto");
        assert_eq!(cursor_model_for(&m, Role::Scout), "auto");
    }

    /// A temp dir holding stub files for `names`, and an `Env` whose `PATH` is only that dir.
    fn stubbed(names: &[&str]) -> (tempfile::TempDir, Env) {
        let dir = tempfile::tempdir().unwrap();
        for n in names {
            let file = if cfg!(windows) {
                format!("{n}.exe")
            } else {
                (*n).to_string()
            };
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        let path = dir.path().to_string_lossy().into_owned();
        (dir, Env::from_pairs([("PATH", path)]))
    }

    #[test]
    fn each_engine_resolves_to_its_own_binary() {
        let (dir, env) = stubbed(&["claude", "cursor-agent", "pi", "codex"]);
        let m = CursorModels::default();
        for (engine, id, stem) in [
            (Engine::Claude, EngineId::Claude, "claude"),
            (Engine::Cursor, EngineId::Cursor, "cursor-agent"),
            (Engine::Pi, EngineId::Pi, "pi"),
            (Engine::Codex, EngineId::Codex, "codex"),
        ] {
            let p = place(engine, Role::Implementer, &env, &m).unwrap();
            assert_eq!(p.engine, id);
            if engine == Engine::Claude && !cfg!(windows) {
                // Claude's bare name is looked up on PATH by `exists`, then spawned by name.
                assert_eq!(p.bin, PathBuf::from("claude"));
            } else {
                assert_eq!(p.bin.parent(), Some(dir.path()));
                assert_eq!(p.bin.file_stem().unwrap(), stem);
            }
        }
    }

    #[test]
    fn a_missing_binary_is_an_error_naming_it() {
        let (_dir, env) = stubbed(&[]);
        let m = CursorModels::default();
        for (engine, name) in [
            (Engine::Cursor, "cursor-agent"),
            (Engine::Pi, "pi"),
            (Engine::Codex, "codex"),
        ] {
            let err = place(engine, Role::Scout, &env, &m).unwrap_err();
            assert!(err.starts_with(&format!("the {name} binary wasn't found (")), "{err}");
        }
    }

    #[test]
    fn local_places_on_claude_with_no_cursor_options() {
        let (_dir, env) = stubbed(&["claude"]);
        let p = place(Engine::Local, Role::Implementer, &env, &CursorModels::default()).unwrap();
        assert_eq!(p.engine, EngineId::Claude);
        assert!(p.cursor.is_none());
        assert!(p.local);
    }

    #[test]
    fn only_local_wants_the_local_model() {
        let (_dir, env) = stubbed(&["claude", "cursor-agent", "pi", "codex"]);
        for engine in [Engine::Claude, Engine::Cursor, Engine::Pi, Engine::Codex] {
            let p = place(engine, Role::Implementer, &env, &CursorModels::default()).unwrap();
            assert!(!p.local, "{engine:?}");
        }
    }

    #[test]
    fn only_cursor_gets_options_with_the_roles_model() {
        let (_dir, env) = stubbed(&["claude", "cursor-agent", "pi", "codex"]);
        let m = models(Some("gpt-5"), None, None);
        for engine in [Engine::Claude, Engine::Pi, Engine::Codex, Engine::Local] {
            assert!(place(engine, Role::Planner, &env, &m).unwrap().cursor.is_none());
        }
        let p = place(Engine::Cursor, Role::Planner, &env, &m).unwrap();
        assert_eq!(p.cursor.unwrap().model.as_deref(), Some("gpt-5"));
    }
}
