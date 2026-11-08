//! The real triager session for `claim` mode (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md): one
//! read-only session on the middle tier, with `budget.reviewerUsd`, in the item's lane if it kept one, else the
//! base checkout (or the repo itself before that checkout exists). Its end is a [`SessionResult::Triage`] on the outcome
//! channel. It runs on the engine chosen for the triager (`engines::place`).
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::driver::{
    LookFailure, SecondLookEnd, SecondLookRequest, SessionOutcome, SessionResult, TriageEnd, TriageRequest,
};
use super::session::{ClaudeStarter, quiet_timeout};
use crate::config::{Engine, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::{
    EphemeralSessionConfig, Placement, Role, TranscriptSink, engine_for, engine_of, place, run_session,
};
use crate::implementer::new_session_id;
use crate::orchestrator::live::sessions::{SessionInfo, model_name};
use crate::tiers::{implementer_tier, mid_tier, top_tier};
use crate::triage::schedule::MAX_TRIAGES;
use crate::triage::second_look::{SecondLook, SecondLookContext, check};
use crate::triage::{Triager, decide};
use crate::{lanes, paths};

const TEMPLATE: &str = include_str!("../../prompts/triage.md");
const CHECK_TEMPLATE: &str = include_str!("../../prompts/triage-check.md");

/// Where the triager reads: the item's lane, else the base checkout, else the repository.
fn cwd_for(project: &ProjectConfig, lane: Option<u32>) -> PathBuf {
    let worktrees = PathBuf::from(&project.worktrees_dir);
    lane.map(|l| lanes::lane_path(&worktrees, l))
        .into_iter()
        .chain([lanes::base_checkout_path(&worktrees)])
        .find(|d| d.is_dir())
        .unwrap_or_else(|| PathBuf::from(&project.repo_path))
}

/// The row a triager session (or its second look) lists itself as.
fn triager_info(project: &str, item: &str, lane: Option<u32>, placed: &Placement, model: &str) -> SessionInfo {
    SessionInfo {
        project: project.to_string(),
        item_id: item.to_string(),
        title: None,
        role: Role::Triager,
        lane,
        engine: placed.engine,
        model: model_name(placed.engine, model, placed.cursor.as_ref()),
    }
}

impl ClaudeStarter {
    /// Where the triager (and its second look) runs: the engine chosen for the role and its binary. With
    /// `engine-placement` Off (`placement`), the engine chosen is not used: Claude alone runs. Only the triager itself is refused then when it isn't on Claude (`start_triage`); a second look always was Claude.
    fn place_triager(&self, project: &ProjectConfig, placement: bool) -> Result<Placement, String> {
        let chosen = engine_for(&self.choice, project.engines.as_ref(), Role::Triager, None);
        place(
            if placement { chosen } else { Engine::Claude },
            Role::Triager,
            &self.env,
            &self.cursor_models,
        )
    }

    /// A new session's id, where its transcript goes, and the running count it's now part of.
    fn begin_session(&self, project: &str, item: &str, lane: Option<u32>) -> (String, TranscriptSink, Arc<AtomicU32>) {
        let session_id = new_session_id();
        let sink = TranscriptSink {
            path: paths::run_dir(&self.data_dir, project, item).join(format!("{session_id}.jsonl")),
            lane,
        };
        let running = self.running.clone();
        running.fetch_add(1, Ordering::SeqCst);
        (session_id, sink, running)
    }

    pub(super) fn start_triage(&self, request: TriageRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        if !request.placement
            && engine_for(&self.choice, project.engines.as_ref(), Role::Triager, None) != Engine::Claude
        {
            return Err("Claude alone starts sessions while engine-placement is off".into());
        }
        let placed = self.place_triager(&project, request.placement)?;
        let TriageRequest {
            project: project_id,
            item_id,
            mut context,
            lane,
            usage,
            outcomes,
            ..
        } = request;
        context.transcripts_dir = Some(
            paths::run_dir(&self.data_dir, &project_id, &item_id)
                .display()
                .to_string(),
        );
        let model = mid_tier(implementer_tier(
            usage.as_ref(),
            crate::telemetry::now_ms() / 1000,
            &self.models,
        ))
        .to_string();
        let (session_id, sink, running) = self.begin_session(&project_id, &item_id, lane);
        let sessions = self.sessions.clone();
        let info = triager_info(&project_id, &item_id, lane, &placed, &model);
        tokio::spawn(async move {
            let cwd = cwd_for(&project, lane);
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let end = match tokio::spawn(async move {
                let triager = Triager {
                    template: TEMPLATE,
                    project_type: project.project_type,
                    github_repo: &project.github_repo,
                    session: EphemeralSessionConfig {
                        cwd: &cwd,
                        budget_usd: project.budget.reviewer_usd,
                        model,
                        local: None,
                        engine: Some(placed.engine),
                        cursor: placed.cursor.clone(),
                        sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
                        shell: true,
                    },
                    max_automatic_decisions: MAX_TRIAGES,
                };
                let engine = engine_of(Some(placed.engine));
                // (hit its budget, hit the usage limit), read off the session's own result.
                let mut cut = (false, false);
                let seen = &mut cut;
                let decided = decide(&triager, &context, |spec| async move {
                    let engine = engine?;
                    let run = run_session(engine.as_ref(), &placed.bin, &spec, &session_id, Some(&sink), quiet);
                    let r = sessions.track(info, run).await?;
                    *seen = (r.subtype == "error_max_budget_usd", r.limit_hit);
                    Ok(r)
                })
                .await;
                match decided.result {
                    Some(r) => TriageEnd::Found {
                        decision: r.decision,
                        reason: r.reason,
                        steps: r.person_steps,
                        instructions: r.instructions,
                        bug: r.retaskable_bug,
                        suggest: r.suggest,
                    },
                    None => TriageEnd::Failed {
                        note: decided.note.unwrap_or_default(),
                        why: match cut {
                            (true, _) => LookFailure::BudgetHit,
                            (_, true) => LookFailure::Limited { resets_in: None },
                            _ => LookFailure::Other,
                        },
                    },
                }
            })
            .await
            {
                Ok(end) => end,
                Err(e) => TriageEnd::Failed {
                    note: format!("the session task crashed: {e}"),
                    why: LookFailure::Other,
                },
            };
            running.fetch_sub(1, Ordering::SeqCst);
            outcomes.send(SessionOutcome {
                project: project_id,
                item_id,
                lane: lane.unwrap_or(0),
                result: SessionResult::Triage(end),
                context_tokens: None,
                usage: None,
                notes: vec![],
                session_id: None,
            });
        });
        Ok(())
    }
}

impl ClaudeStarter {
    /// The second look at a `human` answer with a suggestion (06.9 bullet 3): the triager's role, place and budget on
    /// the top tier, read-only. Its end is a [`SessionResult::SecondLook`]; a session that failed ends with no answer.
    pub(super) fn start_second_look(&self, request: SecondLookRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        let placed = self.place_triager(&project, request.placement)?;
        let SecondLookRequest {
            project: project_id,
            item_id,
            mut context,
            lane,
            outcomes,
            triager_reason,
            triager_suggest,
            triager_instructions,
            ..
        } = request;
        context.transcripts_dir = Some(
            paths::run_dir(&self.data_dir, &project_id, &item_id)
                .display()
                .to_string(),
        );
        let (session_id, sink, running) = self.begin_session(&project_id, &item_id, lane);
        let sessions = self.sessions.clone();
        let info = triager_info(&project_id, &item_id, lane, &placed, top_tier());
        tokio::spawn(async move {
            let cwd = cwd_for(&project, lane);
            let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
            let agree = tokio::spawn(async move {
                let second = SecondLook {
                    template: CHECK_TEMPLATE,
                    project_type: project.project_type,
                    github_repo: &project.github_repo,
                    session: EphemeralSessionConfig {
                        cwd: &cwd,
                        budget_usd: project.budget.reviewer_usd,
                        model: top_tier().to_string(),
                        local: None,
                        engine: Some(placed.engine),
                        cursor: placed.cursor.clone(),
                        sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
                        shell: true,
                    },
                    max_automatic_decisions: MAX_TRIAGES,
                };
                let ctx = SecondLookContext {
                    triage: &context,
                    triager_reason: &triager_reason,
                    triager_suggest,
                    triager_instructions: &triager_instructions,
                };
                let engine = engine_of(Some(placed.engine));
                let checked = check(&second, &ctx, |spec| async move {
                    let engine = engine?;
                    let run = run_session(engine.as_ref(), &placed.bin, &spec, &session_id, Some(&sink), quiet);
                    sessions.track(info, run).await
                })
                .await;
                checked.result.map(|r| (r.agree, r.reason, r.instructions))
            })
            .await
            .unwrap_or(None);
            running.fetch_sub(1, Ordering::SeqCst);
            outcomes.send(SessionOutcome {
                project: project_id,
                item_id,
                lane: lane.unwrap_or(0),
                result: SessionResult::SecondLook(SecondLookEnd { agree }),
                context_tokens: None,
                usage: None,
                notes: vec![],
                session_id: None,
            });
        });
        Ok(())
    }
}
