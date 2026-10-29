//! Triage (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md; Retaskable.com's
//! `agent/src/triage.ts` is the ground truth). When an item goes to `needs_human`, a read-only
//! **triager** session looks into why before a person has to, and answers with a decision the
//! orchestrator would carry out the same way as the buttons on the app: `continue` as Continue,
//! `start_over` as Start over, `skip` as Skip, `human` as leaving it alone, `person_steps` as
//! waiting on a person's own steps.
//!
//! Not wired to anything yet: [`crate::orchestrator`]'s tick only claims a lane for a `queued`
//! item so far, nothing notices a `needs_human` item and runs this on it; [`crate::commands`]'s
//! Continue, Start over and Skip (the buttons this decision would be carried out through) still
//! need the orchestrator loop and lanes too. Also still missing: capping this at 2 automatic
//! decisions a slice and reporting `claudeOnIt` while it's thinking (both need an
//! item-record data model that doesn't exist yet), and actually sending a decided
//! [`bug_report::BugReportMessage`] over the cloud link (reading the hub's own consent and known
//! bugs to decide one with [`bug_report::decide_bug_report`] is as far as this slice goes); no
//! `gh pr view`/`glab mr view` wrapper of our own to gather the PR it names either (the prompt
//! tells the session to run those itself). A caller puts this together the same way as
//! [`crate::ideas`] and [`crate::release_notes::write`]: gather a [`TriageContext`] from the
//! item's own state, build a [`Triager`] with the project's words, the item's lane (or the base
//! checkout) and `budget.reviewerUsd`, and run [`decide`] with
//! [`crate::engines::runner::run_session`]. When that answer is `human` with a `suggest`,
//! [`second_look`] is the top-tier check on it (06.9 bullet 3): run [`second_look::check`] the
//! same way, on the same role, place and budget, then fold whatever it answered (or didn't, on a
//! failure) back onto the triager's own answer with [`second_look::resolve`] before the caller
//! carries anything out.
pub mod bug_report;
pub mod second_look;

use std::future::Future;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::release_notes::{project_words, render_prompt};

/// What the triager is given about the item (06.9 bullet 1): gathered by the caller from the
/// item's own state, not read here.
#[derive(Debug, Clone)]
pub struct TriageContext {
    /// Why the item is `needs_human` right now.
    pub reason: String,
    /// The state it failed in (e.g. "reviewing", "merging") — not `needs_human` itself.
    pub failed_in: String,
    /// The recent timeline, newest first, each one line.
    pub timeline: Vec<String>,
    /// What the last check round reported, if one has run.
    pub last_check_result: Option<String>,
    pub pr_url: Option<String>,
    pub branch: Option<String>,
    pub has_pushed_branch: bool,
    pub has_session_to_continue: bool,
    /// How many automatic decisions this slice has had already, counted across start-overs
    /// (06.9: "at most 2 automatic decisions per slice").
    pub automatic_decisions: u32,
    /// Where this item's session transcripts are, for the triager to read if it wants to.
    pub transcripts_dir: Option<String>,
    /// Bugs already on Retaskable's board that the hub listed (`bug_reports`' `known`), so the
    /// triager can name one in `retaskableBug.sameAs` when this is the same bug worded
    /// differently (06.9 bullet 18).
    pub known_bugs: Vec<KnownBug>,
}

/// One bug already reported, as the hub's `bug_reports` message listed it.
#[derive(Debug, Clone)]
pub struct KnownBug {
    pub id: String,
    pub title: String,
}

/// `decision` (06.9 bullet 2): one of the five the orchestrator carries out the same way as the
/// buttons on the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageDecision {
    Continue,
    StartOver,
    Skip,
    Human,
    PersonSteps,
}

/// `suggest` (06.9's opening line): which button the app would offer first, when the triager
/// leaves the item for a person — only the three a person can actually press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageButton {
    Continue,
    StartOver,
    Skip,
}

impl TriageButton {
    /// The decision this button carries out, the same as the triager's own `continue`/
    /// `start_over`/`skip` (06.9 bullet 3: "carried out like the triager's own decision").
    pub fn decision(self) -> TriageDecision {
        match self {
            TriageButton::Continue => TriageDecision::Continue,
            TriageButton::StartOver => TriageDecision::StartOver,
            TriageButton::Skip => TriageDecision::Skip,
        }
    }

    /// How this button reads as a verb, for the timeline line 06.9 gives: "Claude decided to …,
    /// checked with a second look: …".
    pub fn verb(self) -> &'static str {
        match self {
            TriageButton::Continue => "continue",
            TriageButton::StartOver => "start over",
            TriageButton::Skip => "skip",
        }
    }
}

/// `retaskableBug` (06.9 bullet 18): set only when what went wrong is Retaskable's own, never the
/// project's. [`bug_report::decide_bug_report`] scrubs it and decides whether to send it before it
/// ever leaves this computer.
#[derive(Debug, Clone, Deserialize)]
pub struct RetaskableBug {
    pub title: String,
    pub details: String,
    /// The id of a [`KnownBug`] this is the same bug as, worded differently.
    #[serde(default, rename = "sameAs")]
    pub same_as: Option<String>,
}

/// The structured answer a triager session returns (06.9 bullet 2).
#[derive(Debug, Clone, Deserialize)]
pub struct TriageResult {
    pub decision: TriageDecision,
    #[serde(default, rename = "personSteps")]
    pub person_steps: Vec<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub suggest: Option<TriageButton>,
    #[serde(default, rename = "retaskableBug")]
    pub retaskable_bug: Option<RetaskableBug>,
}

/// For `SessionSpec.schema`.
pub fn triage_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "decision": { "enum": ["continue", "start_over", "skip", "human", "person_steps"] },
            "personSteps": { "type": "array", "items": { "type": "string" } },
            "reason": { "type": "string" },
            "instructions": { "type": "string" },
            "suggest": { "enum": ["continue", "start_over", "skip"] },
            "retaskableBug": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "details": { "type": "string" },
                    "sameAs": { "type": "string" }
                },
                "required": ["title", "details"]
            }
        },
        "required": ["decision", "reason"]
    })
}

/// Fills `prompts/triage.md`'s template with this project's words and the gathered context.
pub fn build_prompt(
    template: &str,
    project_type: ProjectType,
    github_repo: &str,
    max_automatic_decisions: u32,
    ctx: &TriageContext,
) -> Result<String> {
    let words = project_words(project_type);
    let context_json = serde_json::to_string_pretty(&json!({
        "reason": ctx.reason,
        "failedIn": ctx.failed_in,
        "timeline": ctx.timeline,
        "lastCheckResult": ctx.last_check_result,
        "prUrl": ctx.pr_url,
        "branch": ctx.branch,
        "hasPushedBranch": ctx.has_pushed_branch,
        "hasSessionToContinue": ctx.has_session_to_continue,
        "automaticDecisionsUsed": ctx.automatic_decisions,
        "maxAutomaticDecisions": max_automatic_decisions,
        "transcriptsDir": ctx.transcripts_dir,
        "knownBugs": ctx.known_bugs.iter().map(|b| json!({ "id": b.id, "title": b.title })).collect::<Vec<_>>(),
    }))
    .context("rendering triage context as json")?;
    render_prompt(
        template,
        &[
            ("projectKind", words.kind.to_string()),
            ("githubRepo", github_repo.to_string()),
            ("context", context_json),
        ],
    )
}

/// What a triager session runs with, besides the context: the same shape as
/// [`crate::ideas::write::Writer`] and [`crate::release_notes::write::Writer`], plus the one field
/// below that's specific to triage.
pub struct Triager<'a> {
    /// `prompts/triage.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    pub github_repo: &'a str,
    /// `Role::Writer`'s engine/model/budget/local/cursor/sandbox bundle; `session.cwd` is the
    /// item's lane if it kept one, or else the base checkout.
    pub session: EphemeralSessionConfig<'a>,
    /// Told to the model so it knows how much room is left (06.9's "It can't loop"); not
    /// enforced here — the cap across retries is the orchestrator's.
    pub max_automatic_decisions: u32,
}

/// The read-only triager session for one `needs_human` item: nothing to resume, so it's
/// ephemeral.
pub fn session_spec(t: &Triager<'_>, ctx: &TriageContext) -> Result<SessionSpec> {
    let prompt = build_prompt(
        t.template,
        t.project_type,
        t.github_repo,
        t.max_automatic_decisions,
        ctx,
    )?;
    Ok(t.session.spec(prompt, triage_result_schema()))
}

/// What [`decide`] came back with: either the triager's own decision, or why there isn't one
/// (06.9: "If the triager itself fails, the item waits for the human with a note").
#[derive(Debug, Clone)]
pub struct Decided {
    pub result: Option<TriageResult>,
    pub cost_usd: f64,
    /// Set instead of `result` when the session failed or answered with nothing usable.
    pub note: Option<String>,
}

/// Runs the triager session (through `run`, the caller's
/// [`crate::engines::runner::run_session`]) and reads back its decision, falling back to a note
/// for the human on anything that went wrong along the way.
pub async fn decide<F, Fut>(t: &Triager<'_>, ctx: &TriageContext, run: F) -> Decided
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let spec = match session_spec(t, ctx) {
        Ok(spec) => spec,
        Err(e) => {
            return Decided {
                result: None,
                cost_usd: 0.0,
                note: Some(format!("couldn't look into it ({e:#})")),
            };
        }
    };
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => {
            return Decided {
                result: None,
                cost_usd: 0.0,
                note: Some(format!("couldn't look into it ({e:#})")),
            };
        }
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return Decided {
            result: None,
            cost_usd,
            note: Some(describe_failure(&r, t.session.budget_usd)),
        };
    }
    let result = r
        .structured
        .and_then(|v| serde_json::from_value::<TriageResult>(v).ok());
    match result {
        Some(result) => Decided {
            result: Some(result),
            cost_usd,
            note: None,
        },
        None => Decided {
            result: None,
            cost_usd,
            note: Some("didn't answer with a usable decision".to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::engines::SessionStart;
    use crate::engines::engine::test_result as result;

    const TEMPLATE: &str = "Looking at {{projectKind}} (`{{githubRepo}}`):\n{{context}}";

    fn triager() -> Triager<'static> {
        Triager {
            template: TEMPLATE,
            project_type: ProjectType::Web,
            github_repo: "x/y",
            session: EphemeralSessionConfig {
                cwd: Path::new("lane"),
                budget_usd: 1.5,
                model: "sonnet".to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
            },
            max_automatic_decisions: 2,
        }
    }

    fn ctx() -> TriageContext {
        TriageContext {
            reason: "Review escalated: unclear which library to add.".to_string(),
            failed_in: "reviewing".to_string(),
            timeline: vec!["Review requested changes".to_string(), "Checks passed".to_string()],
            last_check_result: Some("passed".to_string()),
            pr_url: Some("https://github.com/x/y/pull/9".to_string()),
            branch: Some("retaskable/9-thing".to_string()),
            has_pushed_branch: true,
            has_session_to_continue: true,
            automatic_decisions: 1,
            transcripts_dir: Some(".retaskable/projects/x/runs/9".to_string()),
            known_bugs: vec![KnownBug {
                id: "retaskable-bug-abc123".to_string(),
                title: "A bug already on the board".to_string(),
            }],
        }
    }

    // build_prompt

    #[test]
    fn build_prompt_fills_every_placeholder_with_the_gathered_context() {
        let prompt = build_prompt(TEMPLATE, ProjectType::Web, "x/y", 2, &ctx()).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("a web app"));
        assert!(prompt.contains("x/y"));
        assert!(prompt.contains("unclear which library to add"));
        assert!(prompt.contains("\"automaticDecisionsUsed\": 1"));
        assert!(prompt.contains("\"maxAutomaticDecisions\": 2"));
        assert!(prompt.contains("retaskable-bug-abc123"));
        assert!(prompt.contains("A bug already on the board"));
    }

    #[test]
    fn build_prompt_fills_the_real_triage_template() {
        let template = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("prompts")
                .join("triage.md"),
        )
        .unwrap();
        let prompt = build_prompt(&template, ProjectType::Web, "x/y", 2, &ctx()).unwrap();
        assert!(!prompt.contains("{{"));
        assert!(prompt.contains("a web app"));
        assert!(prompt.contains("x/y"));
        assert!(prompt.contains("unclear which library to add"));
    }

    #[test]
    fn build_prompt_fails_on_a_template_with_a_placeholder_it_cant_fill() {
        let err = build_prompt("{{unknown}}", ProjectType::Web, "x/y", 2, &ctx()).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    // session_spec

    #[test]
    fn session_spec_is_a_read_only_ephemeral_triager_session_on_the_budget() {
        let spec = session_spec(&triager(), &ctx()).unwrap();
        assert_eq!(spec.model, "sonnet");
        assert_eq!(spec.budget_usd, 1.5);
        assert!(spec.readonly);
        assert_eq!(spec.start, SessionStart::Ephemeral);
        assert_eq!(spec.cwd, Path::new("lane"));
        assert_eq!(spec.schema, triage_result_schema());
        assert!(spec.prompt.contains("x/y"));
    }

    // decide

    #[tokio::test]
    async fn decide_reads_back_the_sessions_structured_decision() {
        let answer = json!({
            "decision": "continue",
            "reason": "A missing import broke the build.",
            "instructions": "Add the import and retry.",
        });
        let decided = decide(&triager(), &ctx(), |spec| async move {
            assert_eq!(spec.model, "sonnet");
            Ok(result(true, "success", Some(answer), None))
        })
        .await;
        let result = decided.result.unwrap();
        assert_eq!(result.decision, TriageDecision::Continue);
        assert_eq!(result.instructions, "Add the import and retry.");
        assert_eq!(decided.cost_usd, 0.12);
        assert!(decided.note.is_none());
    }

    #[tokio::test]
    async fn decide_reads_person_steps_and_the_suggested_button() {
        let answer = json!({
            "decision": "human",
            "reason": "Needs a design call on the empty state.",
            "suggest": "skip",
        });
        let decided = decide(&triager(), &ctx(), |_| async {
            Ok(result(true, "success", Some(answer), None))
        })
        .await;
        let result = decided.result.unwrap();
        assert_eq!(result.decision, TriageDecision::Human);
        assert_eq!(result.suggest, Some(TriageButton::Skip));
    }

    #[tokio::test]
    async fn decide_reads_a_retaskable_bug_naming_a_known_one_its_the_same_as() {
        let answer = json!({
            "decision": "human",
            "reason": "A prompt variable never got filled.",
            "retaskableBug": {
                "title": "A variable isn't filled",
                "details": "Some prompt is missing a value.",
                "sameAs": "retaskable-bug-abc123",
            },
        });
        let decided = decide(&triager(), &ctx(), |_| async {
            Ok(result(true, "success", Some(answer), None))
        })
        .await;
        let bug = decided.result.unwrap().retaskable_bug.unwrap();
        assert_eq!(bug.same_as, Some("retaskable-bug-abc123".to_string()));
    }

    #[tokio::test]
    async fn a_failed_session_leaves_a_note_instead_of_a_decision() {
        let decided = decide(&triager(), &ctx(), |_| async {
            Ok(result(false, "error_max_budget_usd", None, None))
        })
        .await;
        assert!(decided.result.is_none());
        assert_eq!(decided.cost_usd, 0.12);
        assert_eq!(decided.note.as_deref(), Some("hit its $1.5 budget (spent $0.12)"));
    }

    #[tokio::test]
    async fn a_result_with_no_usable_decision_leaves_a_note() {
        let decided = decide(&triager(), &ctx(), |_| async {
            Ok(result(true, "success", None, None))
        })
        .await;
        assert!(decided.result.is_none());
        assert_eq!(decided.note.as_deref(), Some("didn't answer with a usable decision"));
    }

    #[tokio::test]
    async fn a_run_that_cant_even_start_leaves_a_note() {
        let decided = decide(&triager(), &ctx(), |_| async { anyhow::bail!("no claude") }).await;
        assert!(decided.result.is_none());
        assert_eq!(decided.note.as_deref(), Some("couldn't look into it (no claude)"));
        assert_eq!(decided.cost_usd, 0.0);
    }
}
