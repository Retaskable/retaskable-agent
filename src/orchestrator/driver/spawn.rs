//! What starts the orchestrator running (`spawn_all`).
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::MissedTickBehavior;

use super::{OrchestratorMode, ProjectLoop, RunStarter, SessionStarter};
use crate::backlog::Client;
use crate::config::{CursorModels, EngineChoiceConfig, ProjectConfig};
use crate::env::Env;
use crate::link::TokenStore;
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::LiveHandle;
use crate::view::{ItemCommands, PauseState, StorePauseState};

use super::machine::{EVERY, Links, Machine, SetupChecksHandle};

/// What `spawn_all` starts the orchestrator from.
pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub lane_count: u32,
    /// `concurrency`: how many sessions may run at once, across every project.
    pub concurrency: u32,
    /// The computer's local model, shared with the session starters.
    pub local: crate::local_model::LocalModel,
    /// `pollIntervalSec`: how often the loop steps between the minute's passes.
    pub poll_every: Duration,
    pub backlog_url: &'a str,
    pub keychain: &'a Arc<dyn TokenStore>,
    pub paused: &'a PauseState,
    pub store_paused: &'a StorePauseState,
    pub item_commands: &'a ItemCommands,
    pub work: &'a crate::work_signal::WorkSignals,
    pub live: &'a LiveHandle,
    pub guard: Option<&'a PortGuard>,
    pub machine: String,
    pub environment: String,
    pub tags: crate::tags::LiveTags,
    /// Runs sessions for claimed work. None in production until the implementer session is built.
    pub starter: Option<Arc<dyn SessionStarter>>,
    /// Runs spec drafts in `claim` mode. None claims no draft.
    pub drafts: Option<Arc<dyn super::DraftHost>>,
    /// Runs the trial plan in `claim` mode, with the setup checks it waits on. None runs none.
    pub trial: Option<(Arc<dyn crate::orchestrator::trial_host::TrialHost>, SetupChecksHandle)>,
    /// Answers the chat's asks in `claim` mode. None answers none.
    pub asks: Option<Arc<dyn super::AskHost>>,
    /// The runners of `runs` mode, one per set of kinds. Empty until the kind runners are built.
    pub run_starters: Vec<Arc<dyn RunStarter>>,
    /// `MachineConfig::max_resume_context_tokens`: a resumed session over this size starts fresh instead.
    pub max_resume_context_tokens: u32,
    /// Read for `RETASKABLE_CLAUDE`/`PATH` when a playtest grouping round resolves its Claude binary.
    pub env: Env,
    /// Which engine a playtest grouping round's `Role::Planner` session runs on.
    pub engine_choice: EngineChoiceConfig,
    pub cursor_models: CursorModels,
    pub checks: SetupChecksHandle,
    pub bugs: std::collections::HashMap<String, Arc<crate::connector::bug_link::BugLink>>,
    pub bug_version: String,
}

/// Starts the computer's one orchestrator task when the mode isn't `Off`. A project that can't loop (no
/// GitHub repo, unreadable state) is logged and left out; the others go on.
pub fn spawn_all(args: SpawnArgs) {
    if args.live.snapshot().mode == OrchestratorMode::Off {
        return;
    }
    let limits = args.live.sessions().limits();
    {
        // `engine-limit-records`: kept fresh in the background; Off until there is an answer.
        let flag = Arc::new(crate::flags::Flags::from_env());
        let limits = limits.clone();
        tokio::spawn(async move {
            loop {
                limits.record_any_role(flag.on("engine-limit-records").await);
                limits.set_week_cutoff(flag.on("weekly-cutoff").await);
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }
    let mut machine = Machine::new(
        args.lane_count,
        args.concurrency,
        Links {
            backlog_url: args.backlog_url.to_string(),
            keychain: args.keychain.clone(),
            paused: args.paused.clone(),
            store_paused: args.store_paused.clone(),
            item_commands: args.item_commands.clone(),
            work: args.work.clone(),
            machine: args.machine,
            environment: args.environment,
            tags: args.tags,
            env: args.env,
            engine_choice: args.engine_choice,
            cursor_models: args.cursor_models,
            checks: args.checks,
            bugs: args.bugs,
            bug_version: args.bug_version,
        },
    );
    machine.engine_limits = limits;
    machine = machine.with_local(args.local);
    if args.live.snapshot().mode == OrchestratorMode::Runs {
        machine = machine.with_run_starters(args.run_starters);
        let kinds = machine.run_starters.kinds();
        args.live.set_run_kinds(
            kinds
                .iter()
                .filter_map(|k| serde_json::to_value(k).ok()?.as_str().map(String::from))
                .collect(),
        );
    }
    if let Some(starter) = args.starter {
        machine = machine.with_starter(starter);
        if let Some(drafts) = args.drafts {
            machine = machine.with_drafts(drafts);
        }
        if let Some((host, checks)) = args.trial {
            machine = machine.with_trial(host, checks);
        }
        if let Some(asks) = args.asks {
            machine = machine.with_asks(asks);
        }
    } else if args.live.snapshot().mode == OrchestratorMode::Claim {
        tracing::warn!("claim mode: no session runner yet, running dry");
    }
    for project in args.projects {
        let id = project.id_or_derived();
        if project.github_repo.is_empty() {
            tracing::warn!("{id}: orchestrator skipped: the project has no githubRepo, so no backlog repo id");
            continue;
        }
        let client = Client::new(args.backlog_url);
        match ProjectLoop::open(
            args.dir,
            project,
            client,
            args.live.clone(),
            args.guard.cloned(),
            args.max_resume_context_tokens,
        ) {
            Ok(lp) => machine.add(lp),
            Err(e) => tracing::warn!("{id}: orchestrator skipped: {e:#}"),
        }
    }
    if !machine.projects.is_empty() {
        tokio::spawn(run(machine, args.poll_every));
    }
}

/// The poll's period: at least a second. The file's `pollIntervalSec` is used as written (only its environment
/// variable is range-checked), and a hand-edited `0` would panic `tokio::time::interval` and end the loop.
pub(super) fn poll_period(poll_every: Duration) -> Duration {
    poll_every.max(Duration::from_secs(1))
}

/// The loop (section 7's pseudocode): every `poll_every` a poll, and about once a minute (at the first poll
/// too) the fetch and the pass that reads every backlog and writes the `tick`s in its place.
async fn run(mut machine: Machine, poll_every: Duration) {
    let mut every = tokio::time::interval(poll_period(poll_every));
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut minute_at: Option<tokio::time::Instant> = None;
    loop {
        let now = every.tick().await;
        if minute_at.is_none_or(|at| now.duration_since(at) >= EVERY) {
            minute_at = Some(now);
            machine.fetch().await;
            let steps = machine.pass().await;
            machine.playtest_rounds(&steps).await;
            machine.deprecation_checks(&steps).await;
        } else {
            machine.fetch_due().await;
            machine.poll().await;
        }
        machine.run_passes().await;
    }
}
