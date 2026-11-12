//! Running the drafter (spec 06.5 paragraph 3 and step 2, 07.1): its `SessionSpec`, the session's
//! answer parsed back, and `sort_fixes`, which turns the fixes in that answer into store items.
use std::path::Path;

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::model::{DrafterResult, DrafterStatus, Question, drafter_result_schema};
use super::prompt::build_prompt;
use crate::config::{Engine, ProjectBudget};
use crate::decisions::{Decision, decisions_text};
use crate::engines::engine::{CursorOptions, EngineId, LocalTarget};
use crate::engines::{SessionResult, SessionSpec, SessionStart, describe_failure};
use crate::implementer::session::new_session_id;
use crate::lighthouse::is_lighthouse_request;
use crate::tiers::{LOCAL, chore_tier, claude_model, mid_tier};

/// What a drafter session runs with.
pub struct Session<'a> {
    /// `prompts/drafter.md`.
    pub template: &'a str,
    pub request: &'a str,
    /// [`super::prompt::titles_text`]'s output.
    pub titles: &'a str,
    pub branch: &'a str,
    pub base_branch: &'a str,
    /// Appended to a fresh prompt when the draft is tried again after a failure ([`super::failure::retry_text`]).
    pub retry: &'a str,
    /// The engine `engines::engine_for(…, Role::Drafter, None)` chose.
    pub engine: Engine,
    /// `sonnet`, or `local` at Claude's limit (`tiers::floor`); the drafter runs
    /// on [`mid_tier`] of it, so never Opus.
    pub tier: &'static str,
    pub budget: &'a ProjectBudget,
    /// The draft's worktree.
    pub cwd: &'a Path,
    pub local: Option<LocalTarget>,
    /// On Cursor, the `top` model from `engines::place`.
    pub cursor: Option<CursorOptions>,
}

pub enum Start<'a> {
    Fresh,
    /// A follow-up (answers, send-back notes, a rebase request, or "You were interrupted;
    /// continue.") plus what a person has decided since: the whole prompt, never the template.
    Resume {
        session_id: String,
        follow_up: &'a str,
        decisions: &'a [Decision],
    },
}

/// The drafter's tier from the given `tier`: a `[Lighthouse]` draft runs on the chore tier, others on
/// the mid tier (never Opus; `local` stays `local`).
pub fn drafter_tier(request: &str, tier: &'static str) -> &'static str {
    if is_lighthouse_request(request) {
        chore_tier()
    } else {
        mid_tier(tier)
    }
}

/// The drafter's `SessionSpec`: it writes (no code checks, no read-only), in the draft's worktree.
pub fn session_spec(s: &Session<'_>, start: Start<'_>) -> Result<SessionSpec> {
    let (session_start, prompt) = match start {
        Start::Fresh => (
            SessionStart::Fresh(new_session_id()),
            format!(
                "{}{}",
                build_prompt(s.template, s.request, s.titles, s.branch, s.base_branch)?,
                s.retry
            ),
        ),
        Start::Resume {
            session_id,
            follow_up,
            decisions,
        } => (
            SessionStart::Resume(session_id),
            format!("{follow_up}{}", decisions_text(decisions)),
        ),
    };
    let tier = drafter_tier(s.request, s.tier);
    let local_tier = s.engine == Engine::Claude && tier == LOCAL;
    let (engine, model) = match s.engine {
        Engine::Claude => (Some(EngineId::Claude), claude_model(tier)),
        Engine::Cursor => (Some(EngineId::Cursor), tier),
        Engine::Pi => (Some(EngineId::Pi), tier),
        Engine::Codex => (Some(EngineId::Codex), tier),
        Engine::Local => (None, "local"),
    };
    Ok(SessionSpec {
        engine,
        model: model.to_string(),
        prompt,
        schema: drafter_result_schema(),
        cwd: s.cwd.to_path_buf(),
        max_turns: None,
        budget_usd: s.budget.drafter_usd,
        start: session_start,
        readonly: false,
        shell: true,
        cursor: if s.engine == Engine::Cursor {
            s.cursor.clone()
        } else {
            None
        },
        local: if s.engine == Engine::Local || local_tier {
            s.local.clone()
        } else {
            None
        },
        sandbox: false,
    })
}

/// Parses a finished session into the drafter's answer, or says why it isn't one.
pub fn parse_result(r: SessionResult, budget_usd: f64) -> Result<DrafterResult> {
    if !r.ok {
        return Err(anyhow!(describe_failure(&r, budget_usd)));
    }
    let session_id = r.session_id;
    r.structured
        .and_then(|v: Value| serde_json::from_value::<DrafterResult>(v).ok())
        .map(|d| DrafterResult {
            session_id: Some(session_id),
            ..d
        })
        .ok_or_else(|| anyhow!("the drafter finished but didn't answer with the result Retaskable asked for"))
}

/// A fix to add to the store as a `todo` item (`addedVia: chat`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixItem {
    pub id: String,
    /// "Fix: …".
    pub title: String,
    pub remaining: String,
    pub spec_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every piece is a fix: no pull request, and the draft ends done (`queued_fixes`).
    QueuedFixes,
    /// Specs were written: push the branch and open the pull request.
    OpenPr,
    /// Pieces that couldn't be classified, each asked as Fix / Tweak / Feature.
    Ask(Vec<Question>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sorted {
    pub fix_items: Vec<FixItem>,
    pub outcome: Outcome,
}

const KINDS: [&str; 3] = ["fix", "tweak", "feature"];

/// Sorts a finished drafter's pieces (spec 06.5 paragraph 3). Plain code, no model.
pub fn sort_fixes(result: &DrafterResult) -> Sorted {
    let questions: Vec<Question> = result
        .pieces
        .iter()
        .filter(|p| !p.kind.as_deref().is_some_and(|k| KINDS.contains(&k)))
        .map(|p| Question {
            header: p.title.clone(),
            question: format!("Is \"{}\" a fix, a tweak or a feature?", p.title),
            options: vec!["Fix".into(), "Tweak".into(), "Feature".into()],
        })
        .collect();
    if !questions.is_empty() {
        return Sorted {
            fix_items: Vec::new(),
            outcome: Outcome::Ask(questions),
        };
    }
    let fix_items = result
        .fixes
        .iter()
        .map(|f| FixItem {
            id: f.id.clone(),
            title: format!("Fix: {}", f.title.trim_start_matches("Fix: ")),
            remaining: f.remaining.clone(),
            spec_refs: f.spec_refs.clone(),
        })
        .collect::<Vec<_>>();
    let only_fixes = !fix_items.is_empty()
        && result.change_title.is_none()
        && result.entry_ids().is_empty()
        && result.pieces.iter().all(|p| p.kind.as_deref() == Some("fix"));
    let outcome = if only_fixes && result.status == DrafterStatus::PrOpened {
        Outcome::QueuedFixes
    } else {
        Outcome::OpenPr
    };
    Sorted { fix_items, outcome }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn s<'a>(budget: &'a ProjectBudget, cwd: &'a Path) -> Session<'a> {
        Session {
            template: "Request: {{request}}\n{{titles}}\n{{branch}} {{baseBranch}}",
            request: "Add dark mode",
            titles: "- Dark mode",
            branch: "retaskable/spec-1",
            base_branch: "main",
            retry: "",
            engine: Engine::Claude,
            tier: "opus",
            budget,
            cwd,
            local: None,
            cursor: None,
        }
    }

    fn result(v: serde_json::Value) -> DrafterResult {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_lighthouse_draft_runs_on_the_chore_tier_and_others_on_the_usage_tier() {
        let budget = ProjectBudget::default();
        let cwd = Path::new("wt");
        let mut ord = s(&budget, cwd);
        ord.tier = "haiku";
        assert_eq!(session_spec(&ord, Start::Fresh).unwrap().model, "haiku");
        let mut lh = s(&budget, cwd);
        lh.tier = "haiku";
        lh.request = "[Lighthouse] fix it";
        assert_eq!(session_spec(&lh, Start::Fresh).unwrap().model, "sonnet");
    }

    #[test]
    fn a_local_tier_runs_claude_on_sonnet_against_the_local_target_except_for_a_lighthouse_draft() {
        let budget = ProjectBudget::default();
        let cwd = Path::new("wt");
        let target = LocalTarget {
            base_url: "http://x".into(),
            model: "m".into(),
        };
        let mut local = s(&budget, cwd);
        local.tier = "local";
        local.local = Some(target.clone());
        let spec = session_spec(&local, Start::Fresh).unwrap();
        assert_eq!((spec.model.as_str(), spec.engine), ("sonnet", Some(EngineId::Claude)));
        assert_eq!(spec.local, Some(target.clone()));
        local.request = "[Lighthouse] fix it";
        let lh = session_spec(&local, Start::Fresh).unwrap();
        assert_eq!(lh.model, "sonnet");
        assert_eq!(drafter_tier(local.request, "local"), chore_tier());
        let mut cursor = s(&budget, cwd);
        cursor.tier = "sonnet";
        cursor.engine = Engine::Cursor;
        assert_eq!(session_spec(&cursor, Start::Fresh).unwrap().local, None);
    }

    #[test]
    fn the_spec_uses_the_drafter_budget_and_a_tier_that_is_never_opus() {
        let budget = ProjectBudget {
            drafter_usd: 2.0,
            ..Default::default()
        };
        let cwd = Path::new("wt");
        let spec = session_spec(&s(&budget, cwd), Start::Fresh).unwrap();
        assert_eq!(spec.model, "sonnet");
        assert_eq!(spec.budget_usd, 2.0);
        assert_eq!(spec.cwd, cwd);
        assert_eq!(spec.engine, Some(EngineId::Claude));
        assert!(!spec.readonly);
        assert!(matches!(spec.start, SessionStart::Fresh(_)));
        assert!(spec.prompt.contains("Request: Add dark mode") && spec.prompt.contains("- Dark mode"));
    }

    #[test]
    fn a_resumed_prompt_is_the_follow_up_plus_decisions_only() {
        let budget = ProjectBudget::default();
        let cwd = Path::new("wt");
        let spec = session_spec(
            &s(&budget, cwd),
            Start::Resume {
                session_id: "abc".into(),
                follow_up: "You were interrupted; continue.",
                decisions: &[],
            },
        )
        .unwrap();
        assert_eq!(spec.prompt, "You were interrupted; continue.");
        assert!(matches!(spec.start, SessionStart::Resume(ref id) if id == "abc"));
    }

    #[test]
    fn a_fake_engine_result_parses_to_a_drafter_result() {
        let r = SessionResult {
            ok: true,
            subtype: "success".into(),
            structured: Some(json!({"status": "pr_opened", "summary": "done", "changeTitle": "Spec: x"})),
            error: None,
            cost_usd: Some(0.1),
            session_id: "abc".into(),
            ..Default::default()
        };
        let d = parse_result(r, 2.0).unwrap();
        assert_eq!(d.status, DrafterStatus::PrOpened);
        assert_eq!(d.change_title.as_deref(), Some("Spec: x"));
    }

    fn fix(id: &str) -> serde_json::Value {
        json!({"id": id, "title": "Crash on save", "remaining": "It crashes.", "specRefs": ["a.md 1"]})
    }

    #[test]
    fn all_fixes_are_queued_with_no_pr() {
        let d = result(
            json!({"status": "pr_opened", "summary": "s", "fixes": [fix("fix-a")], "pieces": [{"title": "Crash", "kind": "fix"}]}),
        );
        let sorted = sort_fixes(&d);
        assert_eq!(sorted.outcome, Outcome::QueuedFixes);
        assert_eq!(sorted.fix_items[0].title, "Fix: Crash on save");
    }

    #[test]
    fn fixes_beside_specs_open_a_pr_and_still_add_the_fix_items() {
        let d = result(
            json!({"status": "pr_opened", "summary": "s", "changeTitle": "Spec: x", "itemIds": ["x"],
            "fixes": [fix("fix-a")], "pieces": [{"title": "Crash", "kind": "fix"}, {"title": "Dark", "kind": "feature"}]}),
        );
        let sorted = sort_fixes(&d);
        assert_eq!(sorted.outcome, Outcome::OpenPr);
        assert_eq!(sorted.fix_items.len(), 1);
    }

    #[test]
    fn an_unclassified_piece_is_asked_as_fix_tweak_or_feature() {
        let d = result(json!({"status": "pr_opened", "summary": "s", "fixes": [fix("fix-a")],
            "pieces": [{"title": "Crash", "kind": "fix"}, {"title": "Odd"}, {"title": "Bad", "kind": "chore"}]}));
        let Outcome::Ask(qs) = sort_fixes(&d).outcome else {
            panic!("expected a question")
        };
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header, "Odd");
        assert_eq!(qs[0].options, ["Fix", "Tweak", "Feature"]);
    }
}
