//! One round of ideas under the project's `ideas` lock (docs/specs/agent/09-web-ui.md section 9.3, "Ideas";
//! `write()` in Retaskable.com's agent/src/ideas.ts): reads the store's ideas, runs the read-only `writer`
//! session through [`crate::ideas::run_round`] and sends the new queue with `ideas_round`. Nothing schedules
//! it yet: [`run`] is what a daily job or a person's ask will call.
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use crate::backlog::Client;
use crate::backlog::asks::Audience;
use crate::backlog::runs::LockTake;
use crate::config::{Engine, EngineChoiceConfig, ProjectConfig};
use crate::engines::claude::sandbox_enabled;
use crate::engines::resolve::resolve_claude_on_path;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Role, SessionResult, SessionSpec, engine_for, engine_of, run_session,
};
use crate::env::Env;
use crate::ideas::{
    IDEAS_LOCK, IDEAS_MAX_WAITS_DEFAULT, Ideas, QUEUE, Writer, profiles_for, queue_from_store, round_context, run_round,
};
use crate::link::TokenStore;
use crate::orchestrator::session::{exists, quiet_timeout};
use crate::runs::RunLocks;

const TEMPLATE: &str = include_str!("../../../prompts/ideas.md");

/// What one round needs, gathered like [`super::playtest_round::Job`]; `live` and `audiences` are what the
/// caller read for the prompt (`gather_live`, the store's audiences).
pub(super) struct Job {
    pub id: String,
    pub client: Arc<Client>,
    pub repo: Option<u64>,
    pub project: ProjectConfig,
    pub env: Env,
    pub engine_choice: EngineChoiceConfig,
    pub machine: String,
    pub backlog_url: String,
    pub keychain: Arc<dyn TokenStore>,
    /// The project's held locks in `runs` mode (the lock is `lock_take`n into it); `None` in `claim` mode.
    pub locks: Option<RunLocks>,
    /// The planner's directory; the session reads there when it exists, else in the repo.
    pub planner_dir: Option<PathBuf>,
    pub live: Vec<String>,
    pub audiences: Vec<Audience>,
    /// `ideasQueue` and `ideasMaxWaits` from the project's config, when it sets them.
    pub queue: Option<usize>,
    pub max_waits: Option<u32>,
}

/// Why this round runs.
pub(super) struct Ask {
    /// Start from an empty queue: only the new ideas are sent.
    pub fresh: bool,
    /// A daily round, not a person's ask: another computer's lock just ends it.
    pub quiet: bool,
    /// What prompted the round (e.g. a merged title).
    pub after: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    Ok {
        model: String,
        cost_usd: f64,
    },
    Skipped(String),
    Failed {
        error: String,
        model: String,
        cost_usd: f64,
    },
}

fn failed(error: String) -> Outcome {
    Outcome::Failed {
        error,
        model: String::new(),
        cost_usd: 0.0,
    }
}

/// [`run_with`] on the real engine. Only Claude runs a writer session yet.
pub(super) async fn run(job: Job, ask: Ask) -> Outcome {
    let engine = engine_for(&job.engine_choice, job.project.engines.as_ref(), Role::Writer, None);
    if engine != Engine::Claude {
        return Outcome::Skipped("only Claude runs an ideas session yet".into());
    }
    let bin = match resolve_claude_on_path(&job.env) {
        Ok(bin) if exists(&bin, &job.env) => bin,
        Ok(_) => return Outcome::Skipped("the claude binary wasn't found".into()),
        Err(e) => return Outcome::Skipped(format!("{e:#}")),
    };
    let quiet = quiet_timeout(job.project.tuning.session_quiet_minutes);
    run_with(job, ask, |spec| async move {
        let engine = engine_of(Some(EngineId::Claude))?;
        run_session(engine.as_ref(), &bin, &spec, "ideas", None, quiet).await
    })
    .await
}

/// Takes the lock, runs the round through `runner` and sends its ideas; always drops the lock.
pub(super) async fn run_with<F, Fut>(job: Job, ask: Ask, runner: F) -> Outcome
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let outcome = round(&job, &ask, runner).await;
    match &outcome {
        Outcome::Ok { model, cost_usd } => {
            tracing::info!("{}: ideas round done on {model} (${cost_usd:.2})", job.id)
        }
        Outcome::Skipped(why) => tracing::info!("{}: ideas round skipped: {why}", job.id),
        Outcome::Failed { error, .. } => tracing::warn!("{}: ideas round failed: {error}", job.id),
    }
    outcome
}

async fn round<F, Fut>(job: &Job, ask: &Ask, runner: F) -> Outcome
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let Some(repo) = job.repo else {
        return Outcome::Skipped("no repo id yet".into());
    };
    let token = match job.keychain.get(&job.backlog_url) {
        Ok(Some(token)) => token,
        Ok(None) => return Outcome::Skipped("not linked to the shared backlog".into()),
        Err(e) => return Outcome::Skipped(format!("reading the backlog link: {e:#}")),
    };
    let taken = match take_lock(job, repo, &token).await {
        Ok(taken) => taken,
        Err(e) => return failed(format!("Couldn't take the ideas lock ({e})")),
    };
    let lock_token = match taken {
        Taken::Mine(lock_token) => lock_token,
        Taken::Other(by) => {
            return Outcome::Skipped(if ask.quiet {
                "another computer is already thinking of ideas".into()
            } else {
                match by {
                    Some(by) if !by.is_empty() => {
                        format!("@{by}'s computer is already thinking of ideas: they'll show here when it's done.")
                    }
                    _ => "Another computer is already thinking of ideas: they'll show here when it's done.".into(),
                }
            });
        }
    };
    let outcome = think(job, ask, repo, &token, runner).await;
    match (&job.locks, &lock_token) {
        (Some(locks), Some(lock)) => {
            if let Err(e) = job.client.lock_drop(repo, Some(&token), lock).await {
                tracing::warn!("{}: dropping the ideas lock failed: {e}", job.id);
            }
            locks.remove(lock);
        }
        _ => {
            if let Err(e) = job.client.lock(repo, Some(&token), IDEAS_LOCK, false).await {
                tracing::warn!("{}: dropping the ideas lock failed: {e}", job.id);
            }
        }
    }
    outcome
}

enum Taken {
    /// Ours; the token in `runs` mode.
    Mine(Option<String>),
    Other(Option<String>),
}

async fn take_lock(job: &Job, repo: u64, token: &str) -> Result<Taken, crate::backlog::BacklogError> {
    match &job.locks {
        None => {
            let answer = job.client.lock(repo, Some(token), IDEAS_LOCK, true).await?;
            Ok(if answer.by.as_deref() == Some(job.machine.as_str()) {
                Taken::Mine(None)
            } else {
                Taken::Other(answer.by)
            })
        }
        Some(locks) => {
            let note = format!("ideas on {}", job.machine);
            Ok(
                match job.client.lock_take(repo, Some(token), IDEAS_LOCK, Some(&note)).await? {
                    LockTake::Held(lock) => {
                        locks.add(&lock, IDEAS_LOCK, Instant::now());
                        Taken::Mine(Some(lock))
                    }
                    LockTake::HeldBy { by, .. } => Taken::Other(Some(by)),
                },
            )
        }
    }
}

async fn think<F, Fut>(job: &Job, ask: &Ask, repo: u64, token: &str, runner: F) -> Outcome
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let store = Ideas {
        client: &job.client,
        repo_id: repo,
        token: Some(token),
    };
    let listed = match store.list().await {
        Ok(listed) => listed,
        Err(e) => return failed(format!("Couldn't read the ideas from the shared backlog ({e})")),
    };
    let mut was = queue_from_store(&listed);
    if ask.fresh {
        was.ideas.clear();
    }
    let context = round_context(&job.live, &listed);
    let profiles = profiles_for(&job.audiences);
    let cwd = match &job.planner_dir {
        Some(dir) if dir.exists() => dir.clone(),
        _ => PathBuf::from(&job.project.repo_path),
    };
    let writer = Writer {
        template: TEMPLATE,
        project_type: job.project.project_type,
        github_repo: &job.project.github_repo,
        session: EphemeralSessionConfig {
            cwd: &cwd,
            budget_usd: job.project.budget.scout_usd,
            model: "sonnet".to_string(),
            local: None,
            engine: Some(EngineId::Claude),
            cursor: None,
            sandbox: sandbox_enabled(job.project.sandbox, cfg!(windows)),
            shell: false,
        },
    };
    let stamp = chrono::Utc::now().to_rfc3339();
    let after = ask.after.as_deref();
    let done = run_round(
        &was,
        &writer,
        after.unwrap_or(""),
        &profiles,
        &context,
        job.queue.unwrap_or(QUEUE),
        job.max_waits.unwrap_or(IDEAS_MAX_WAITS_DEFAULT),
        &stamp,
        runner,
    )
    .await;
    if let Some(error) = done.error {
        return Outcome::Failed {
            error,
            model: done.model,
            cost_usd: done.cost_usd,
        };
    }
    if let Err(e) = store.round(&done.ideas, after).await {
        return Outcome::Failed {
            error: format!("Couldn't send the ideas to the shared backlog ({e})"),
            model: done.model,
            cost_usd: done.cost_usd,
        };
    }
    Outcome::Ok {
        model: done.model,
        cost_usd: done.cost_usd,
    }
}

#[cfg(test)]
#[path = "ideas_round_tests.rs"]
mod tests;
