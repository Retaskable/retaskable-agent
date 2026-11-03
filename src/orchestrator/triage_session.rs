//! The real triager session for `claim` mode (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md): one
//! read-only Claude session on the middle tier, with `budget.reviewerUsd`, in the item's lane if it kept one, else the
//! base checkout (or the repo itself before that checkout exists). Its end is a [`SessionResult::Triage`] on the outcome
//! channel. Claude only, like the implementer.
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use super::driver::{LookFailure, SessionOutcome, SessionResult, TriageEnd, TriageRequest};
use super::session::{ClaudeStarter, exists, quiet_timeout};
use crate::config::{Engine, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{EngineId, EphemeralSessionConfig, Role, TranscriptSink, engine_for, engine_of, run_session};
use crate::implementer::new_session_id;
use crate::tiers::{implementer_tier, mid_tier};
use crate::triage::schedule::MAX_TRIAGES;
use crate::triage::{Triager, decide};
use crate::{lanes, paths};

const TEMPLATE: &str = include_str!("../../prompts/triage.md");

/// Where the triager reads: the item's lane, else the base checkout, else the repository.
fn cwd_for(project: &ProjectConfig, lane: Option<u32>) -> PathBuf {
    let worktrees = PathBuf::from(&project.worktrees_dir);
    lane.map(|l| lanes::lane_path(&worktrees, l))
        .into_iter()
        .chain([lanes::base_checkout_path(&worktrees)])
        .find(|d| d.is_dir())
        .unwrap_or_else(|| PathBuf::from(&project.repo_path))
}

impl ClaudeStarter {
    pub(super) fn start_triage(&self, request: TriageRequest) -> Result<(), String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id_or_derived() == request.project)
            .ok_or_else(|| format!("no project {} is configured", request.project))?
            .clone();
        if engine_for(&self.choice, project.engines.as_ref(), Role::Triager, None) != Engine::Claude {
            return Err("only Claude sessions run in the Rust agent yet".into());
        }
        let bin = resolve_claude_on_path(&self.env).map_err(|e| format!("{e:#}"))?;
        if !exists(&bin, &self.env) {
            return Err(format!("the claude binary wasn't found ({})", bin.display()));
        }
        let TriageRequest {
            project: project_id,
            item_id,
            mut context,
            lane,
            usage,
            outcomes,
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
        let session_id = new_session_id();
        let sink = TranscriptSink {
            path: paths::run_dir(&self.data_dir, &project_id, &item_id).join(format!("{session_id}.jsonl")),
            lane,
        };
        let running = self.running.clone();
        running.fetch_add(1, Ordering::SeqCst);
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
                        engine: Some(EngineId::Claude),
                        cursor: None,
                        sandbox: sandbox_enabled(project.sandbox, cfg!(windows)),
                        shell: true,
                    },
                    max_automatic_decisions: MAX_TRIAGES,
                };
                let engine = engine_of(Some(EngineId::Claude));
                // (hit its budget, hit the usage limit), read off the session's own result.
                let mut cut = (false, false);
                let seen = &mut cut;
                let decided = decide(&triager, &context, |spec| async move {
                    let r = run_session(engine?.as_ref(), &bin, &spec, &session_id, Some(&sink), quiet).await?;
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
