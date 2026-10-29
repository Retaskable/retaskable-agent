//! The second look (06.9 bullet 3, `agent/src/triage.ts`'s second-look check): when a triager's
//! `human` decision names a `suggest`, this top-tier, read-only session checks whether that
//! button is clearly safe to press instead of waiting for a person. [`check`] runs the session
//! and reads back its answer the same way [`super::decide`] does the triager's own; [`resolve`]
//! is the pure code afterwards — folding an agreed suggestion into the decision to carry out, or
//! a disagreement (or a failed look) back into the item's reason — the same way
//! [`super::bug_report::decide_bug_report`] decides a bug report before anything sends it.
//! Running the session at all, and carrying out whatever [`resolve`] answers, is still the
//! orchestrator's once it exists (see the module doc on [`crate::triage`]).

use std::future::Future;

use anyhow::{Context as _, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::release_notes::{project_words, render_prompt};

use super::{TriageButton, TriageContext, TriageDecision, TriageResult};

/// Whether a triager's answer gets a second look at all (06.9 bullet 3): only `human` with a
/// `suggest` does — `suggest: "none"`, `person_steps` and the triager's own `continue`/
/// `start_over`/`skip` never do.
pub fn needs_second_look(result: &TriageResult) -> bool {
    result.decision == TriageDecision::Human && result.suggest.is_some()
}

/// What the second-look session reads, besides the facts the triager had: its own reason,
/// suggestion and instructions (06.9: "It gets what the triager got plus the triager's reason,
/// suggestion and instructions").
#[derive(Debug, Clone)]
pub struct SecondLookContext<'a> {
    pub triage: &'a TriageContext,
    pub triager_reason: &'a str,
    pub triager_suggest: TriageButton,
    pub triager_instructions: &'a str,
}

/// The structured answer a second-look session returns (06.9 bullet 3).
#[derive(Debug, Clone, Deserialize)]
pub struct SecondLookResult {
    pub agree: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub instructions: String,
}

/// For `SessionSpec.schema`.
pub fn second_look_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agree": { "type": "boolean" },
            "reason": { "type": "string" },
            "instructions": { "type": "string" }
        },
        "required": ["agree", "reason"]
    })
}

/// Fills `prompts/triage-check.md` with this project's words, the facts the triager had (06.9:
/// "it gets what the triager got"), and the triager's own answer.
pub fn build_prompt(
    template: &str,
    project_type: ProjectType,
    github_repo: &str,
    max_automatic_decisions: u32,
    ctx: &SecondLookContext<'_>,
) -> Result<String> {
    let words = project_words(project_type);
    let t = ctx.triage;
    let context_json = serde_json::to_string_pretty(&json!({
        "reason": t.reason,
        "failedIn": t.failed_in,
        "timeline": t.timeline,
        "lastCheckResult": t.last_check_result,
        "prUrl": t.pr_url,
        "branch": t.branch,
        "hasPushedBranch": t.has_pushed_branch,
        "hasSessionToContinue": t.has_session_to_continue,
        "automaticDecisionsUsed": t.automatic_decisions,
        "maxAutomaticDecisions": max_automatic_decisions,
        "transcriptsDir": t.transcripts_dir,
        "triagerReason": ctx.triager_reason,
        "triagerSuggest": ctx.triager_suggest,
        "triagerInstructions": ctx.triager_instructions,
    }))
    .context("rendering second-look context as json")?;
    render_prompt(
        template,
        &[
            ("projectKind", words.kind.to_string()),
            ("githubRepo", github_repo.to_string()),
            ("context", context_json),
        ],
    )
}

/// What a second-look session runs with, besides the context: the same role, place and budget as
/// the triager that flagged it (06.9 bullet 3: "same role, read-only session, place and budget"),
/// just the top tier's model.
pub struct SecondLook<'a> {
    /// `prompts/triage-check.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    pub github_repo: &'a str,
    pub session: EphemeralSessionConfig<'a>,
    /// Told to the session the same way it was told to the triager (06.9's "It can't loop").
    pub max_automatic_decisions: u32,
}

/// The read-only second-look session for one triager answer: nothing to resume, so it's
/// ephemeral.
pub fn session_spec(s: &SecondLook<'_>, ctx: &SecondLookContext<'_>) -> Result<SessionSpec> {
    let prompt = build_prompt(
        s.template,
        s.project_type,
        s.github_repo,
        s.max_automatic_decisions,
        ctx,
    )?;
    Ok(s.session.spec(prompt, second_look_result_schema()))
}

/// What [`check`] came back with: either the session's own answer, or why there isn't one.
#[derive(Debug, Clone)]
pub struct Checked {
    pub result: Option<SecondLookResult>,
    pub cost_usd: f64,
    /// Set instead of `result` when the session failed or answered with nothing usable — 06.9
    /// treats that the same as `human` on its own, which [`resolve`] already does for a `None`
    /// result.
    pub note: Option<String>,
}

/// Runs the second-look session (through `run`, the caller's
/// [`crate::engines::runner::run_session`]) and reads back its answer.
pub async fn check<F, Fut>(s: &SecondLook<'_>, ctx: &SecondLookContext<'_>, run: F) -> Checked
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let spec = match session_spec(s, ctx) {
        Ok(spec) => spec,
        Err(e) => {
            return Checked {
                result: None,
                cost_usd: 0.0,
                note: Some(format!("couldn't check it ({e:#})")),
            };
        }
    };
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => {
            return Checked {
                result: None,
                cost_usd: 0.0,
                note: Some(format!("couldn't check it ({e:#})")),
            };
        }
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return Checked {
            result: None,
            cost_usd,
            note: Some(describe_failure(&r, s.session.budget_usd)),
        };
    }
    let result = r
        .structured
        .and_then(|v| serde_json::from_value::<SecondLookResult>(v).ok());
    match result {
        Some(result) => Checked {
            result: Some(result),
            cost_usd,
            note: None,
        },
        None => Checked {
            result: None,
            cost_usd,
            note: Some("didn't answer with a usable check".to_string()),
        },
    }
}

/// What a triager's `human` decision, with a `suggest`, becomes once a second look has checked
/// it (06.9 bullet 3) — pure code the caller applies to whatever [`check`] (or a session
/// [`needs_second_look`] says never had to run) came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The decision to carry out: the triager's own, or the suggested button once the second
    /// look agreed.
    pub decision: TriageDecision,
    pub instructions: String,
    /// What goes in the item's timeline alongside the decision.
    pub reason: String,
    /// Still offered first by the app when the decision stayed `human`.
    pub suggest: Option<TriageButton>,
}

/// `second_look` is `None` both when no second look was needed ([`needs_second_look`] was false)
/// and when one ran but failed — 06.9 says a failed look "is `human` as above", the same as one
/// that was never needed: nothing added to the triager's own reason.
pub fn resolve(triage: &TriageResult, second_look: Option<&SecondLookResult>) -> Resolved {
    let unchanged = Resolved {
        decision: triage.decision,
        instructions: triage.instructions.clone(),
        reason: triage.reason.clone(),
        suggest: triage.suggest,
    };
    let Some(suggest) = triage.suggest.filter(|_| needs_second_look(triage)) else {
        return unchanged;
    };
    let Some(look) = second_look else {
        return unchanged;
    };
    let reason = look.reason.trim();
    if look.agree {
        Resolved {
            decision: suggest.decision(),
            instructions: if look.instructions.trim().is_empty() {
                triage.instructions.clone()
            } else {
                look.instructions.clone()
            },
            reason: format!(
                "Claude decided to {}, checked with a second look: {reason}",
                suggest.verb()
            ),
            suggest: None,
        }
    } else {
        Resolved {
            decision: TriageDecision::Human,
            instructions: triage.instructions.clone(),
            reason: format!("{}\nA second look kept it for you: {reason}", triage.reason.trim()),
            suggest: Some(suggest),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::engines::SessionStart;
    use crate::engines::engine::test_result as result;

    const TEMPLATE: &str = "Checking {{projectKind}} (`{{githubRepo}}`):\n{{context}}";

    fn triage_ctx() -> TriageContext {
        crate::test_support::triage_context(vec!["Review requested changes".to_string()], "A known bug")
    }

    fn triage_result(suggest: TriageButton) -> TriageResult {
        TriageResult {
            decision: TriageDecision::Human,
            person_steps: Vec::new(),
            reason: "Needs a design call on the empty state.".to_string(),
            instructions: "Pick the simpler empty state.".to_string(),
            suggest: Some(suggest),
            retaskable_bug: None,
        }
    }

    fn second_look() -> SecondLook<'static> {
        SecondLook {
            template: TEMPLATE,
            project_type: ProjectType::Web,
            github_repo: "x/y",
            session: EphemeralSessionConfig {
                cwd: Path::new("lane"),
                budget_usd: 1.5,
                model: "opus".to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
            },
            max_automatic_decisions: 2,
        }
    }

    fn ctx<'a>(triage: &'a TriageContext, triager_result: &'a TriageResult) -> SecondLookContext<'a> {
        SecondLookContext {
            triage,
            triager_reason: &triager_result.reason,
            triager_suggest: triager_result.suggest.unwrap(),
            triager_instructions: &triager_result.instructions,
        }
    }

    // needs_second_look

    #[test]
    fn only_human_with_a_suggest_needs_a_second_look() {
        assert!(needs_second_look(&triage_result(TriageButton::Skip)));
        let mut no_suggest = triage_result(TriageButton::Skip);
        no_suggest.suggest = None;
        assert!(!needs_second_look(&no_suggest));
        let mut not_human = triage_result(TriageButton::Skip);
        not_human.decision = TriageDecision::Continue;
        assert!(!needs_second_look(&not_human));
    }

    // build_prompt

    #[test]
    fn build_prompt_fills_every_placeholder_with_the_triagers_own_answer() {
        let triage = triage_ctx();
        let triager_result = triage_result(TriageButton::StartOver);
        let prompt = build_prompt(TEMPLATE, ProjectType::Web, "x/y", 2, &ctx(&triage, &triager_result)).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("a web app"));
        assert!(prompt.contains("x/y"));
        assert!(prompt.contains("design call on the empty state"));
        assert!(prompt.contains("\"triagerSuggest\": \"start_over\""));
        assert!(prompt.contains("Pick the simpler empty state."));
        assert!(prompt.contains("\"automaticDecisionsUsed\": 1"));
        assert!(prompt.contains("\"maxAutomaticDecisions\": 2"));
        assert!(prompt.contains(".retaskable/projects/x/runs/9"));
    }

    #[test]
    fn build_prompt_fills_the_real_triage_check_template() {
        let template = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("triage-check.md"),
        )
        .unwrap();
        let triage = triage_ctx();
        let triager_result = triage_result(TriageButton::Continue);
        let prompt = build_prompt(&template, ProjectType::Web, "x/y", 2, &ctx(&triage, &triager_result)).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("x/y"));
    }

    // session_spec

    #[test]
    fn session_spec_is_a_read_only_ephemeral_check_session_on_the_budget() {
        let triage = triage_ctx();
        let triager_result = triage_result(TriageButton::Skip);
        let spec = session_spec(&second_look(), &ctx(&triage, &triager_result)).unwrap();
        assert_eq!(spec.model, "opus");
        assert_eq!(spec.budget_usd, 1.5);
        assert!(spec.readonly);
        assert_eq!(spec.start, SessionStart::Ephemeral);
        assert_eq!(spec.cwd, Path::new("lane"));
        assert_eq!(spec.schema, second_look_result_schema());
    }

    // check

    #[tokio::test]
    async fn check_reads_back_the_sessions_structured_answer() {
        let answer = json!({ "agree": true, "reason": "One clear path.", "instructions": "" });
        let triage = triage_ctx();
        let triager_result = triage_result(TriageButton::Continue);
        let checked = check(&second_look(), &ctx(&triage, &triager_result), |spec| async move {
            assert_eq!(spec.model, "opus");
            Ok(result(true, "success", Some(answer), None))
        })
        .await;
        let r = checked.result.unwrap();
        assert!(r.agree);
        assert_eq!(r.reason, "One clear path.");
        assert_eq!(checked.cost_usd, 0.12);
        assert!(checked.note.is_none());
    }

    #[tokio::test]
    async fn a_failed_session_leaves_a_note_instead_of_an_answer() {
        let triage = triage_ctx();
        let triager_result = triage_result(TriageButton::Skip);
        let checked = check(&second_look(), &ctx(&triage, &triager_result), |_| async {
            Ok(result(false, "error_max_budget_usd", None, None))
        })
        .await;
        assert!(checked.result.is_none());
        assert_eq!(checked.note.as_deref(), Some("hit its $1.5 budget (spent $0.12)"));
    }

    #[tokio::test]
    async fn an_answer_with_nothing_usable_leaves_a_note() {
        let triage = triage_ctx();
        let triager_result = triage_result(TriageButton::Skip);
        let checked = check(&second_look(), &ctx(&triage, &triager_result), |_| async {
            Ok(result(true, "success", None, None))
        })
        .await;
        assert!(checked.result.is_none());
        assert_eq!(checked.note.as_deref(), Some("didn't answer with a usable check"));
    }

    // resolve

    #[test]
    fn an_agreed_suggestion_is_carried_out_like_the_triagers_own_decision() {
        let triage = triage_result(TriageButton::Continue);
        let look = SecondLookResult {
            agree: true,
            reason: "One clear path, nothing to decide.".to_string(),
            instructions: String::new(),
        };
        let resolved = resolve(&triage, Some(&look));
        assert_eq!(resolved.decision, TriageDecision::Continue);
        assert_eq!(resolved.instructions, triage.instructions);
        assert_eq!(
            resolved.reason,
            "Claude decided to continue, checked with a second look: One clear path, nothing to decide."
        );
        assert_eq!(resolved.suggest, None);
    }

    #[test]
    fn an_agreed_suggestion_with_its_own_instructions_replaces_the_triagers() {
        let triage = triage_result(TriageButton::StartOver);
        let look = SecondLookResult {
            agree: true,
            reason: "Safe to restart.".to_string(),
            instructions: "Start from a clean branch this time.".to_string(),
        };
        let resolved = resolve(&triage, Some(&look));
        assert_eq!(resolved.decision, TriageDecision::StartOver);
        assert_eq!(resolved.instructions, "Start from a clean branch this time.");
    }

    #[test]
    fn a_disagreement_stays_human_with_its_reason_kept_under_the_triagers() {
        let triage = triage_result(TriageButton::Skip);
        let look = SecondLookResult {
            agree: false,
            reason: "This touches a decision the specs already made.".to_string(),
            instructions: String::new(),
        };
        let resolved = resolve(&triage, Some(&look));
        assert_eq!(resolved.decision, TriageDecision::Human);
        assert_eq!(
            resolved.reason,
            "Needs a design call on the empty state.\nA second look kept it for you: This touches a decision the specs already made."
        );
        assert_eq!(resolved.suggest, Some(TriageButton::Skip));
    }

    #[test]
    fn a_failed_look_stays_human_with_nothing_added() {
        let triage = triage_result(TriageButton::Skip);
        let resolved = resolve(&triage, None);
        assert_eq!(resolved.decision, TriageDecision::Human);
        assert_eq!(resolved.reason, triage.reason);
        assert_eq!(resolved.suggest, Some(TriageButton::Skip));
    }

    #[test]
    fn a_decision_that_never_needed_a_second_look_passes_through_unchanged() {
        let mut triage = triage_result(TriageButton::Skip);
        triage.suggest = None;
        let look = SecondLookResult {
            agree: true,
            reason: "ignored".to_string(),
            instructions: String::new(),
        };
        // Even an (impossible in practice) `Some` is ignored when there was nothing to check.
        let resolved = resolve(&triage, Some(&look));
        assert_eq!(resolved.decision, TriageDecision::Human);
        assert_eq!(resolved.reason, triage.reason);
        assert_eq!(resolved.suggest, None);
    }
}
