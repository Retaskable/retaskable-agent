//! Running the assistant (spec 06.6 paragraph 2, 07.1): its read-only `SessionSpec` and the
//! session's answer parsed back into the answer text, the checked actions and the dropped ones.
use std::path::Path;

use anyhow::Result;

use super::model::{AssistantResult, assistant_result_schema, parse_assistant_result};
use super::prompt::{AssistantInput, build_prompt};
use crate::config::{Engine, ProjectBudget};
use crate::engines::engine::{EngineId, LocalTarget, ephemeral_readonly_spec};
use crate::engines::{SessionResult, SessionSpec, describe_failure};

/// What an assistant session runs with.
pub struct Session<'a> {
    /// `prompts/assistant.md`.
    pub template: &'a str,
    pub input: AssistantInput<'a>,
    /// The engine `engines::engine_for(…, Role::Assistant, None)` chose.
    pub engine: Engine,
    /// The implementer's tier from the usage reading (`tiers::implementer_tier`); the assistant
    /// runs on it as is, not on `mid_tier`.
    pub tier: &'static str,
    pub budget: &'a ProjectBudget,
    /// The project's base checkout, on the base branch.
    pub cwd: &'a Path,
    pub local: Option<LocalTarget>,
}

/// The assistant's `SessionSpec`: fresh, read-only, `budget.ask_usd`, in the base checkout.
pub fn session_spec(s: &Session<'_>) -> Result<SessionSpec> {
    let engine = match s.engine {
        Engine::Claude => Some(EngineId::Claude),
        Engine::Cursor => Some(EngineId::Cursor),
        Engine::Pi => Some(EngineId::Pi),
        Engine::Codex => Some(EngineId::Codex),
        Engine::Local => None,
    };
    let model = if engine.is_some() { s.tier } else { "local" };
    Ok(ephemeral_readonly_spec(
        engine,
        model.to_string(),
        build_prompt(s.template, &s.input)?,
        assistant_result_schema(),
        s.cwd.to_path_buf(),
        s.budget.ask_usd,
        None,
        if s.engine == Engine::Local {
            s.local.clone()
        } else {
            None
        },
        false,
        true,
    ))
}

/// Parses a finished session into the assistant's answer, or a short failure answer a person can
/// read in the chat.
pub fn parse_result(r: SessionResult, budget_usd: f64) -> AssistantResult {
    let answer = if r.ok {
        match r.structured.as_ref().and_then(parse_assistant_result) {
            Some(result) => return result,
            None => "I couldn't put together an answer. Please try asking again.".to_string(),
        }
    } else {
        format!("I couldn't answer: the assistant {}.", describe_failure(&r, budget_usd))
    };
    AssistantResult {
        answer,
        actions: vec![],
        dropped: vec![],
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ask::model::Action;
    use crate::ask::prompt::StateSummary;
    use crate::config::ProjectType;
    use crate::engines::SessionStart;

    fn input(state: &StateSummary) -> AssistantInput<'_> {
        AssistantInput {
            project_type: ProjectType::default(),
            base_branch: "main",
            github_repo: "o/r",
            pr: "pull request",
            change: "pull request",
            state,
            audiences: &[],
            asks: &[],
            reply_to: None,
            question: "why is step 5 stuck?",
        }
    }

    fn spec_for(engine: Engine, tier: &'static str) -> SessionSpec {
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
        session_spec(&Session {
            template: "Q: {{question}}",
            input: input(&state),
            engine,
            tier,
            budget: &budget,
            cwd: Path::new("base"),
            local: None,
        })
        .unwrap()
    }

    #[test]
    fn the_spec_is_read_only_on_the_implementer_tier_in_the_base_checkout() {
        let spec = spec_for(Engine::Claude, "opus");
        assert!(spec.readonly);
        assert_eq!(spec.model, "opus");
        assert_eq!(spec.engine, Some(EngineId::Claude));
        assert_eq!(spec.budget_usd, 1.5);
        assert_eq!(spec.cwd, Path::new("base"));
        assert_eq!(spec.schema, assistant_result_schema());
        assert!(matches!(spec.start, SessionStart::Ephemeral));
        assert!(spec.prompt.contains("Q: why is step 5 stuck?"));
    }

    #[test]
    fn each_engine_gets_the_tier_and_local_gets_local() {
        assert_eq!(spec_for(Engine::Codex, "sonnet").engine, Some(EngineId::Codex));
        assert_eq!(spec_for(Engine::Pi, "sonnet").model, "sonnet");
        let local = spec_for(Engine::Local, "opus");
        assert_eq!((local.engine, local.model.as_str()), (None, "local"));
    }

    #[test]
    fn a_good_result_parses_to_answer_actions_and_dropped() {
        let r = SessionResult {
            ok: true,
            subtype: "success".into(),
            structured: Some(json!({"answer": "Pausing.", "actions": [{"type": "pause"}, {"type": "launch_rockets"}]})),
            ..Default::default()
        };
        let got = parse_result(r, 1.5);
        assert_eq!(got.answer, "Pausing.");
        assert_eq!(got.actions, vec![Action::Pause]);
        assert_eq!(got.dropped.len(), 1);
    }

    #[test]
    fn a_failed_session_gives_a_readable_failure_answer() {
        let r = SessionResult {
            ok: false,
            subtype: "error_max_budget_usd".into(),
            cost_usd: Some(1.6),
            ..Default::default()
        };
        let got = parse_result(r, 1.5);
        assert!(got.answer.contains("hit its $1.5 budget"), "{}", got.answer);
        assert!(got.actions.is_empty() && got.dropped.is_empty());
    }

    #[test]
    fn an_answer_without_the_result_is_a_failure_answer_too() {
        let r = SessionResult {
            ok: true,
            structured: Some(json!({"nope": 1})),
            ..Default::default()
        };
        assert!(parse_result(r, 1.5).answer.contains("couldn't"));
    }
}
