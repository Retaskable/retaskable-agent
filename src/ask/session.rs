//! Running the assistant (spec 06.6 paragraph 2, 07.1): its read-only `SessionSpec` and the session's
//! answer parsed back into the answer, the checked actions and the dropped ones.
use std::path::Path;

use anyhow::Result;

use super::model::{AssistantResult, assistant_result_schema, parse_assistant_result};
use super::prompt::{AssistantInput, build_prompt};
use crate::config::{Engine, ProjectBudget};
use crate::engines::engine::{CursorOptions, EngineId, LocalTarget};
use crate::engines::{SessionResult, SessionSpec, describe_failure, ephemeral_readonly_spec};

/// What an assistant session runs with.
pub struct Session<'a> {
    /// `prompts/assistant.md`.
    pub template: &'a str,
    pub input: AssistantInput<'a>,
    /// The engine `engines::engine_for(…, Role::Ask, None)` chose.
    pub engine: Engine,
    /// The implementer's tier from the usage reading (`tiers::implementer_tier`), used as it is.
    pub tier: &'static str,
    pub budget: &'a ProjectBudget,
    /// The project's base checkout, on the base branch.
    pub cwd: &'a Path,
    pub local: Option<LocalTarget>,
    /// Only on Cursor: the `scout` model name (`engines::place`).
    pub cursor: Option<CursorOptions>,
}

/// The assistant's `SessionSpec`: fresh and read-only, in the base checkout.
pub fn session_spec(s: &Session<'_>) -> Result<SessionSpec> {
    let (engine, model) = match s.engine {
        Engine::Claude => (Some(EngineId::Claude), s.tier),
        Engine::Cursor => (Some(EngineId::Cursor), s.tier),
        Engine::Pi => (Some(EngineId::Pi), s.tier),
        Engine::Codex => (Some(EngineId::Codex), s.tier),
        Engine::Local => (None, "local"),
    };
    Ok(ephemeral_readonly_spec(
        engine,
        model.to_string(),
        build_prompt(s.template, &s.input)?,
        assistant_result_schema(),
        s.cwd.to_path_buf(),
        s.budget.ask_usd,
        s.cursor.clone(),
        if s.engine == Engine::Local {
            s.local.clone()
        } else {
            None
        },
        false,
        true,
    ))
}

/// Parses a finished session into the assistant's result. A session that failed, ran over budget or
/// answered with nothing usable gives a short answer saying so, with no actions.
pub fn parse_result(r: SessionResult, budget_usd: f64) -> AssistantResult {
    let failed = |why: String| AssistantResult {
        answer: format!("Sorry, I couldn't answer that: the assistant {why}."),
        actions: vec![],
        dropped: vec![],
    };
    if !r.ok {
        return failed(describe_failure(&r, budget_usd));
    }
    match r.structured.as_ref().and_then(parse_assistant_result) {
        Some(a) => a,
        None => failed("finished without an answer".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ask::model::Action;
    use crate::ask::prompt::StateSummary;
    use crate::engines::SessionStart;
    use crate::tiers::mid_tier;

    fn with_session<T>(engine: Engine, tier: &'static str, f: impl FnOnce(SessionSpec) -> T) -> T {
        with_cursor(engine, tier, None, f)
    }

    fn with_cursor<T>(
        engine: Engine,
        tier: &'static str,
        cursor: Option<CursorOptions>,
        f: impl FnOnce(SessionSpec) -> T,
    ) -> T {
        let budget = ProjectBudget {
            ask_usd: 1.5,
            ..Default::default()
        };
        let state = StateSummary {
            about: None,
            items: vec![],
            lanes: vec![],
            drafts: vec![],
            status: "running".into(),
            paused_reason: None,
            spent_today_usd: 0.0,
            daily_usd: 10.0,
        };
        let s = Session {
            template: "Q: {{question}}",
            input: AssistantInput {
                project_type: Default::default(),
                base_branch: "main",
                github_repo: "o/r",
                pr: "PR",
                change: "pull request",
                state: &state,
                audiences: &[],
                asks: &[],
                reply_to: None,
                question: "why is step 5 stuck?",
            },
            engine,
            tier,
            budget: &budget,
            cwd: Path::new("base"),
            local: None,
            cursor,
        };
        f(session_spec(&s).unwrap())
    }

    #[test]
    fn the_spec_is_read_only_on_the_implementer_tier_in_the_base_checkout() {
        with_session(Engine::Claude, "opus", |spec| {
            assert!(spec.readonly);
            assert_eq!(spec.model, "opus");
            assert_ne!(spec.model, mid_tier("opus"));
            assert_eq!(spec.engine, Some(EngineId::Claude));
            assert_eq!(spec.budget_usd, 1.5);
            assert_eq!(spec.cwd, Path::new("base"));
            assert_eq!(spec.schema, assistant_result_schema());
            assert!(!matches!(spec.start, SessionStart::Resume(_)));
            assert!(spec.prompt.contains("why is step 5 stuck?"));
        });
    }

    #[test]
    fn each_engine_gets_its_own_id_and_local_gets_no_engine() {
        with_session(Engine::Codex, "sonnet", |s| {
            assert_eq!((s.engine, s.model.as_str()), (Some(EngineId::Codex), "sonnet"));
        });
        with_session(Engine::Local, "opus", |s| {
            assert_eq!((s.engine, s.model.as_str()), (None, "local"));
        });
    }

    #[test]
    fn a_cursor_session_carries_its_model_name() {
        let cursor = CursorOptions {
            model: Some("fast".into()),
            ..CursorOptions::default()
        };
        with_cursor(Engine::Cursor, "sonnet", Some(cursor), |s| {
            assert_eq!(s.engine, Some(EngineId::Cursor));
            assert_eq!(s.cursor.unwrap().model.as_deref(), Some("fast"));
        });
        with_session(Engine::Claude, "sonnet", |s| assert!(s.cursor.is_none()));
    }

    fn done(v: serde_json::Value) -> SessionResult {
        SessionResult {
            ok: true,
            subtype: "success".into(),
            structured: Some(v),
            ..Default::default()
        }
    }

    #[test]
    fn a_good_result_parses_to_answer_actions_and_dropped() {
        let r = parse_result(
            done(json!({"answer": "Paused it.", "actions": [{"type": "pause"}, {"type": "bogus"}]})),
            1.0,
        );
        assert_eq!(r.answer, "Paused it.");
        assert_eq!(r.actions, vec![Action::Pause]);
        assert_eq!(r.dropped.len(), 1);
    }

    #[test]
    fn a_failed_session_gives_a_readable_answer() {
        let r = parse_result(
            SessionResult {
                ok: false,
                subtype: "error_max_budget_usd".into(),
                cost_usd: Some(1.2),
                ..Default::default()
            },
            1.0,
        );
        assert!(r.answer.contains("hit its $1 budget"), "{}", r.answer);
        assert!(r.actions.is_empty());
        assert!(
            parse_result(done(json!({"nope": 1})), 1.0)
                .answer
                .contains("without an answer")
        );
    }
}
