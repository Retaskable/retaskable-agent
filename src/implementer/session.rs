//! Running the implementer (section 6.2's prompt-building and the "fresh vs. follow-up" shape in
//! `agent/src/steps.ts`): a fresh session gets the full rendered template plus "Where to start"
//! (`super::prompt::build_prompt`); a follow-up to one already running gets just the new message,
//! with any answers a person gave appended (`decisions_text`) — never re-templated.
use std::future::Future;
use std::path::Path;

use anyhow::{Result, anyhow, bail};

use crate::backlog::Item;
use crate::config::{CheckConfig, CodeGates, ProjectType};
use crate::decisions::{Decision, decisions_text};
use crate::engines::engine::{CursorOptions, EngineId, LocalTarget};
use crate::engines::{SessionResult, SessionSpec, SessionStart, describe_failure};

use super::model::{ImplementerResult, implementer_result_schema};
use super::prompt::{Prompt, build_prompt};

/// What an implementer session runs with, besides how it starts ([`Start`]).
pub struct Session<'a> {
    /// `prompts/implementer.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    pub item: &'a Item,
    pub branch: &'a str,
    pub base_branch: &'a str,
    pub github_repo: &'a str,
    pub checks: &'a [CheckConfig],
    pub code_gates: CodeGates,
    /// [`super::start_files::render`]'s output, or empty for a session that needs none yet. Only
    /// read on a [`Start::Fresh`] session: a follow-up's prompt is just the new message.
    pub where_to_start: &'a str,
    pub cwd: &'a Path,
    pub model: String,
    pub budget_usd: f64,
    pub max_turns: Option<u32>,
    pub engine: Option<EngineId>,
    pub cursor: Option<CursorOptions>,
    pub local: Option<LocalTarget>,
    pub sandbox: bool,
}

/// How this session begins (`SessionStart`, keyed to this one item rather than a bare id/prompt
/// pair, so the caller can't accidentally re-template a follow-up).
pub enum Start<'a> {
    /// A brand new session: Retaskable names its id itself ([`new_session_id`]) so a later
    /// follow-up can resume it.
    Fresh,
    /// A follow-up to a session already running: `follow_up` plus whatever a person has decided
    /// since (`decisions_text`) is the whole prompt — never the template again.
    Resume {
        session_id: String,
        follow_up: &'a str,
        decisions: &'a [Decision],
    },
}

/// A fresh v4 UUID (`crypto.randomUUID()`'s shape), hand-rolled from `rand` rather than adding a
/// `uuid` dependency this agent otherwise has no use for.
pub fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    for b in &mut bytes {
        *b = rand::random::<u8>();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The `SessionSpec` for `s`, either starting fresh (templating the full prompt) or resuming one
/// already running (the follow-up text alone).
pub fn session_spec(s: &Session<'_>, start: Start<'_>) -> Result<SessionSpec> {
    let (session_start, prompt) = match start {
        Start::Fresh => {
            let prompt = build_prompt(
                s.template,
                &Prompt {
                    item: s.item,
                    project_type: s.project_type,
                    branch: s.branch,
                    base_branch: s.base_branch,
                    github_repo: s.github_repo,
                    checks: s.checks,
                    code_gates: s.code_gates,
                    where_to_start: s.where_to_start,
                },
            )?;
            (SessionStart::Fresh(new_session_id()), prompt)
        }
        Start::Resume {
            session_id,
            follow_up,
            decisions,
        } => (
            SessionStart::Resume(session_id),
            format!("{follow_up}{}", decisions_text(decisions)),
        ),
    };
    Ok(SessionSpec {
        engine: s.engine,
        model: s.model.clone(),
        prompt,
        schema: implementer_result_schema(),
        cwd: s.cwd.to_path_buf(),
        max_turns: s.max_turns,
        budget_usd: s.budget_usd,
        start: session_start,
        readonly: false,
        cursor: s.cursor.clone(),
        local: s.local.clone(),
        sandbox: s.sandbox,
    })
}

/// Marks a failed run's error as the quiet-output watchdog's own kill (`killed_for_silence`,
/// `engines::runner`'s `spawn_and_read`), rather than the implementer giving up: the session may
/// have just stalled on something slow, so the caller treats this like an own failure (back to the
/// queue) instead of escalating to a person (`orchestrator::session::run`'s `downcast_ref`).
#[derive(Debug)]
pub struct SilenceKilled(pub String);

impl std::fmt::Display for SilenceKilled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SilenceKilled {}

/// Runs `spec` through `run` (the caller's [`crate::engines::run_session`]) and parses its
/// structured answer. A session that failed, or answered with something other than the result
/// Retaskable asked for, is an error: unlike a release note, there's no plain fallback for code
/// that was never written — the caller (not yet built: the orchestrator loop) decides whether to
/// retry, escalate, or ask a person from there, except a watchdog kill ([`SilenceKilled`]), which
/// already says which.
pub async fn run_implementer<F, Fut>(spec: SessionSpec, run: F) -> Result<ImplementerResult>
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let budget_usd = spec.budget_usd;
    let r = run(spec).await?;
    if !r.ok {
        let why = describe_failure(&r, budget_usd);
        if r.subtype == "killed_for_silence" {
            return Err(anyhow::Error::new(SilenceKilled(why)));
        }
        bail!(why);
    }
    r.structured
        .and_then(|v| serde_json::from_value::<ImplementerResult>(v).ok())
        .ok_or_else(|| anyhow!("the session finished but didn't answer with the result Retaskable asked for"))
}

#[cfg(test)]
mod tests {
    use regex::Regex;
    use serde_json::json;

    use super::*;
    use crate::implementer::test_support::{checks, item};

    fn session<'a>(it: &'a Item, chks: &'a [CheckConfig], cwd: &'a Path) -> Session<'a> {
        Session {
            template: "Item {{itemId}}, slice {{slice}}: {{remaining}}\n{{checks}}{{codeLimits}}",
            project_type: ProjectType::Cli,
            item: it,
            branch: "retaskable/implementer-session-s2",
            base_branch: "main",
            github_repo: "Retaskable/retaskable-agent",
            checks: chks,
            code_gates: CodeGates::default(),
            where_to_start: "## Where to start\n\n- `src/implementer/mod.rs`",
            cwd,
            model: "sonnet".to_string(),
            budget_usd: 5.0,
            max_turns: None,
            engine: None,
            cursor: None,
            local: None,
            sandbox: false,
        }
    }

    fn result(ok: bool, subtype: &str, structured: Option<serde_json::Value>) -> SessionResult {
        SessionResult {
            ok,
            subtype: subtype.to_string(),
            structured,
            error: None,
            cost_usd: Some(1.5),
            session_id: "s".to_string(),
            limit_hit: false,
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: None,
        }
    }

    #[test]
    fn new_session_id_is_a_version_4_variant_1_uuid() {
        let pattern = Regex::new("^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").unwrap();
        let a = new_session_id();
        let b = new_session_id();
        assert!(pattern.is_match(&a), "{a} doesn't look like a v4 uuid");
        assert_ne!(a, b);
    }

    #[test]
    fn a_fresh_session_spec_templates_the_full_prompt_under_a_new_id() {
        let it = item(&["docs/specs/agent/06.2-implementer.md"], Some("Wire the session."));
        let cwd = Path::new("repo");
        let spec = session_spec(&session(&it, &checks(), cwd), Start::Fresh).unwrap();
        assert!(!spec.readonly);
        assert_eq!(spec.schema, implementer_result_schema());
        assert_eq!(spec.cwd, cwd);
        assert!(
            spec.prompt
                .starts_with("Item implementer-session, slice 2: Wire the session.")
        );
        assert!(spec.prompt.contains("cargo test"));
        assert!(spec.prompt.ends_with("- `src/implementer/mod.rs`"));
        match spec.start {
            SessionStart::Fresh(id) => assert_eq!(id.len(), 36),
            other => panic!("expected a fresh session, got {other:?}"),
        }
    }

    #[test]
    fn a_resumed_session_spec_is_just_the_follow_up_and_any_decisions() {
        let it = item(&[], None);
        let cwd = Path::new("repo");
        let decisions = vec![Decision {
            question: "Squash or merge?".to_string(),
            answer: "Squash".to_string(),
            at: "t".to_string(),
        }];
        let spec = session_spec(
            &session(&it, &[], cwd),
            Start::Resume {
                session_id: "abc-123".to_string(),
                follow_up: "The checks failed: fix the lint error.",
                decisions: &decisions,
            },
        )
        .unwrap();
        assert_eq!(spec.start, SessionStart::Resume("abc-123".to_string()));
        assert!(spec.prompt.starts_with("The checks failed: fix the lint error."));
        assert!(spec.prompt.contains("Q: Squash or merge?\n  A: Squash"));
        assert!(!spec.prompt.contains("{{"), "a resumed prompt is never re-templated");
    }

    #[tokio::test]
    async fn run_implementer_parses_the_structured_answer() {
        let it = item(&[], None);
        let cwd = Path::new("repo");
        let spec = session_spec(&session(&it, &[], cwd), Start::Fresh).unwrap();
        let r = run_implementer(spec, |spec| async move {
            assert!(!spec.readonly);
            Ok(result(
                true,
                "success",
                Some(json!({ "status": "pr_opened", "summary": "Wired the session." })),
            ))
        })
        .await
        .unwrap();
        assert_eq!(r.status, "pr_opened");
        assert_eq!(r.summary, "Wired the session.");
    }

    #[tokio::test]
    async fn run_implementer_fails_on_a_failed_session_or_a_missing_answer() {
        let it = item(&[], None);
        let cwd = Path::new("repo");

        let spec = session_spec(&session(&it, &[], cwd), Start::Fresh).unwrap();
        let err = run_implementer(spec, |_| async { Ok(result(false, "error_max_budget_usd", None)) })
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "hit its $5 budget (spent $1.50)");

        let spec = session_spec(&session(&it, &[], cwd), Start::Fresh).unwrap();
        let err = run_implementer(spec, |_| async { Ok(result(true, "success", None)) })
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the session finished but didn't answer with the result Retaskable asked for"
        );
    }

    #[tokio::test]
    async fn a_watchdog_kill_is_tagged_as_silence_killed_not_a_plain_failure() {
        let it = item(&[], None);
        let cwd = Path::new("repo");

        let spec = session_spec(&session(&it, &[], cwd), Start::Fresh).unwrap();
        let err = run_implementer(spec, |_| async { Ok(result(false, "killed_for_silence", None)) })
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<SilenceKilled>().is_some(),
            "a watchdog kill should be distinguishable from an ordinary failure, got {err:#}"
        );

        let spec = session_spec(&session(&it, &[], cwd), Start::Fresh).unwrap();
        let err = run_implementer(spec, |_| async { Ok(result(false, "error_during_execution", None)) })
            .await
            .unwrap_err();
        assert!(err.downcast_ref::<SilenceKilled>().is_none());
    }
}
