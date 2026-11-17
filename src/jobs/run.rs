//! Running a task's job (`runJob` in `jobTasks.ts`): the kind picks the entry point, the computer's own settings and
//! tools fill its input, and what it answers is the end for `task_end`. Never panics on a kind it doesn't know: that
//! is a refusal in words. Decides nothing about when or whether: the task is the store's word.
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::a11y::{A11yInput, a11y_job};
use super::deprecations::{GitSpecs, deprecations_job};
use super::host::{JobHost, JobRole, Tools};
use super::inputs::{a11y_settings, scan_settings, staging_token};
use super::lighthouse::{LighthouseInput, LighthouseSettings, lighthouse_job};
use super::lighthouse_reports::ReportFlush;
use super::probes::{ProbesInput, probes_job};
use super::release::{ReleaseArgs, RepoGather, release_job};
use super::store::StoreAccess;
use super::triage::{TriageEnv, triage_job};
use super::vet::vet_job;
use super::vet_wire::VetWires;
use super::{ChromeFlag, JobKind, JobOutcome, JobSpec, Outcome, run_failed, unknown_kind};
use crate::config::ProjectConfig;
use crate::connector::bug_link::BugLink;
use crate::env::Env;
use crate::release_notes::{DEFAULT_FULL_ENTRIES, DEFAULT_MAX_PRS, Writer};
use crate::triage::bug_report::{decide_bug_report, scrub_words};

/// What a project gives its jobs besides the task.
pub struct JobCtx<'a> {
    pub project_id: &'a str,
    pub project: &'a ProjectConfig,
    pub chrome: &'a ChromeFlag,
    pub flush: &'a ReportFlush,
    /// The project's own folder in the data dir (`lighthouse/<day>/` lives in it).
    pub project_dir: &'a Path,
    pub store: Option<StoreAccess>,
    pub hub: Option<Arc<BugLink>>,
    pub env: &'a Env,
    /// What a bug report says the build is.
    pub bug_version: &'a str,
    /// Where an item's session transcripts are, for triage.
    pub runs_dir: String,
    /// When a usage limit lifts (epoch ms), for a session that hit one.
    pub limit_resume_ms: i64,
    /// What this computer holds of the base branch, for the `main` job to fill.
    pub main: &'a crate::main_look::MainLook,
    /// The facts this computer writes down for the store (the baseline, CI and CI coverage results).
    pub facts: &'a crate::kept_facts::KeptFacts,
}

fn gh_bin(env: &Env) -> std::path::PathBuf {
    env.get("RETASKABLE_GH").map_or_else(|| "gh".into(), Into::into)
}

/// The job `spec` names, run now. Never fails: an error is a failed run, in words.
pub async fn run_job(ctx: &JobCtx<'_>, host: &dyn JobHost, spec: &JobSpec) -> JobOutcome {
    let Some(kind) = JobKind::parse(&spec.kind) else {
        return unknown_kind(&spec.kind);
    };
    let tools = Tools(host);
    let cfg = ctx.project;
    match kind {
        JobKind::Lighthouse => {
            let settings = LighthouseSettings::from_config(cfg);
            let input = LighthouseInput {
                job: spec,
                settings: &settings,
                project_dir: ctx.project_dir,
                known: Default::default(),
                chrome: ctx.chrome,
            };
            lighthouse_job(&input, &tools, ctx.store.as_ref(), ctx.flush)
                .await
                .into_job()
        }
        JobKind::Probes => {
            let settings = scan_settings(cfg);
            // The test token is read here, at the run, so it is never in the target or the result.
            let token_for = |origin: &str| staging_token(ctx.env, origin);
            let input = ProbesInput {
                job: spec,
                settings: &settings,
                token_for: &token_for,
                known: Default::default(),
            };
            probes_job(&input, &tools).await.into_job()
        }
        JobKind::A11y => {
            let settings = a11y_settings(cfg);
            let input = A11yInput {
                job: spec,
                settings: &settings,
                known: Default::default(),
                chrome: ctx.chrome,
            };
            a11y_job(&input, &tools).await.into_job()
        }
        JobKind::Deprecations => {
            let specs = GitSpecs {
                repo_path: cfg.repo_path.clone().into(),
                default_branch: Some(cfg.base_branch.clone()),
            };
            match tokio::task::spawn_blocking(move || deprecations_job(&specs)).await {
                Ok(out) => out.into_job(),
                Err(e) => run_failed::<Value>(&spec.kind, &e),
            }
        }
        JobKind::Vet => vet(ctx, host, spec).await,
        JobKind::Triage => triage(ctx, host, spec).await,
        JobKind::ReleaseNotes => release(ctx, host, spec).await,
        JobKind::Main => {
            let gh = gh_bin(ctx.env);
            let look = crate::main_look::Look {
                id: ctx.project_id,
                project: cfg,
                held: ctx.main,
                facts: ctx.facts,
                gh: &gh,
            };
            crate::main_look::main_job(&look, spec).await
        }
    }
}

async fn vet(ctx: &JobCtx<'_>, host: &dyn JobHost, spec: &JobSpec) -> JobOutcome {
    let session = match host.session(ctx.project, JobRole::Vet).await {
        Ok(s) => s,
        Err(why) => return run_failed(&spec.kind, &why),
    };
    let wires = VetWires {
        hub: ctx.hub.clone(),
        store: ctx.store.clone(),
        session: &*session,
    };
    vet_job(&ctx.project.github_repo, &wires).await.into_job()
}

async fn triage(ctx: &JobCtx<'_>, host: &dyn JobHost, spec: &JobSpec) -> JobOutcome {
    let check = spec.args.get("look").and_then(Value::as_str) == Some("check");
    let role = if check {
        JobRole::TriageCheck
    } else {
        JobRole::TriageLook
    };
    let session = match host.session(ctx.project, role).await {
        Ok(s) => s,
        Err(why) => return run_failed(&spec.kind, &why),
    };
    let cfg = ctx.project;
    let known = ctx.hub.as_ref().map(|h| h.known()).unwrap_or_default();
    let session_cfg = session.config().ephemeral();
    let limit = ctx.limit_resume_ms;
    let env = TriageEnv {
        has_checkout: !cfg.repo_path.trim().is_empty(),
        base_branch: &cfg.base_branch,
        code_host_kind: cfg.code_host.as_ref().map_or("", |c| c.kind.as_str()),
        branches_here: false,
        runs_dir: &ctx.runs_dir,
        known_bugs: &known,
        session: &session_cfg,
        limit_resume_ms: &move || limit,
    };
    let run = triage_job(spec, &env, |s| session.run(s)).await;
    if let Some(bug) = &run.bug {
        send_bug(ctx, bug).await;
    }
    run.outcome.into_job()
}

/// A Retaskable bug the first look named goes to the hub scrubbed, whatever the look decided (`reportLookBug`).
async fn send_bug(ctx: &JobCtx<'_>, bug: &super::triage::BugToReport) {
    let Some(link) = ctx.hub.clone() else {
        tracing::info!(
            "{}: no bug report for {}: the project has no hub link",
            ctx.project_id,
            bug.id
        );
        return;
    };
    let words = scrub_words(ctx.project_id, ctx.project, &bug.id, bug.branch.as_deref(), ctx.env);
    let message = decide_bug_report(
        Some(&bug.bug),
        Some(&bug.why),
        &words,
        link.consent(),
        None,
        &link.known(),
    );
    let Some(message) = message else {
        tracing::info!(
            "{}: no bug report sent for {}: not agreed or empty",
            ctx.project_id,
            bug.id
        );
        return;
    };
    // Sent from its own task: the look's end is not held for the hub.
    let version = ctx.bug_version.to_string();
    tokio::spawn(async move {
        link.send(&message, &version).await;
    });
}

async fn release(ctx: &JobCtx<'_>, host: &dyn JobHost, spec: &JobSpec) -> JobOutcome {
    let cfg = ctx.project;
    let has_checkout = !cfg.repo_path.trim().is_empty();
    // A task with no range or no checkout is refused before anything is read.
    if !has_checkout || ReleaseArgs::from_job(spec).is_none() {
        let none = |_| async { Err(anyhow::anyhow!("no session is needed to refuse")) };
        let w = Writer {
            template: super::release::TEMPLATE,
            project_type: cfg.project_type,
            base_branch: &cfg.base_branch,
            github_repo: &cfg.github_repo,
            cwd: Path::new(&cfg.repo_path),
            max_full_entries: DEFAULT_FULL_ENTRIES,
            budget_usd: 0.0,
            model: String::new(),
            local: None,
            engine: None,
            cursor: None,
            sandbox: false,
        };
        let gather = RepoGather {
            gh_bin: Path::new("gh"),
            git_bin: crate::git_bin::program(),
            repo_path: Path::new(&cfg.repo_path),
            github_repo: &cfg.github_repo,
            base_branch: &cfg.base_branch,
            max_prs: DEFAULT_MAX_PRS,
            items: &[],
            drafts: &[],
        };
        return release_job(spec, has_checkout, &gather, &w, none).await.into_job();
    }
    let Some(store) = &ctx.store else {
        return Outcome::refused("This computer isn't linked to the backlog store.");
    };
    let (items, drafts) = match tokio::join!(store.items(), store.drafts()) {
        (Ok(i), Ok(d)) => (i, d),
        (Err(e), _) | (_, Err(e)) => return run_failed(&spec.kind, &e),
    };
    let session = match host.session(cfg, JobRole::Release).await {
        Ok(s) => s,
        Err(why) => return run_failed(&spec.kind, &why),
    };
    let c = session.config();
    let writer = Writer {
        template: super::release::TEMPLATE,
        project_type: cfg.project_type,
        base_branch: &cfg.base_branch,
        github_repo: &cfg.github_repo,
        cwd: &c.cwd,
        max_full_entries: DEFAULT_FULL_ENTRIES,
        budget_usd: c.budget_usd,
        model: c.model.clone(),
        local: c.local.clone(),
        engine: c.engine,
        cursor: c.cursor.clone(),
        sandbox: c.sandbox,
    };
    let gh = gh_bin(ctx.env);
    let gather = RepoGather {
        gh_bin: &gh,
        git_bin: crate::git_bin::program(),
        repo_path: Path::new(&cfg.repo_path),
        github_repo: &cfg.github_repo,
        base_branch: &cfg.base_branch,
        max_prs: DEFAULT_MAX_PRS,
        items: &items,
        drafts: &drafts,
    };
    release_job(spec, true, &gather, &writer, |s| session.run(s))
        .await
        .into_job()
}
