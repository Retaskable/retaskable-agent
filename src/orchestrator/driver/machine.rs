//! The computer's orchestrator: one shared lane pool and every project's state, and what starts the
//! whole loop running (`spawn_all`).
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use tokio::sync::mpsc;

use super::panic::panic_message;
use super::read::{Fail, Read, read};
use super::run_starters::RunStarters;
use super::{OrchestratorMode, OutcomeSender, ProjectLoop, RunStarter, SessionStarter, Step};
use crate::backlog::Item;
use crate::backlog::access::AccessTokens;
use crate::base_hold;
use crate::config::{CursorModels, EngineChoiceConfig};
use crate::env::Env;
use crate::lane_pool::{LanePool, PoolRules};
use crate::lanes;
use crate::orchestrator::Action;
use crate::orchestrator::fresh_start::EngineLimits;
use crate::orchestrator::round_robin::{Candidate, plan};
use crate::process_tree;
use crate::telemetry::{Event, event};
use crate::view::{ItemCommands, PauseState, StorePause, StorePauseState};

/// How often the loop reads every project's backlog and writes its `tick` events: the TS agent's once a
/// minute (`MINUTE_MS` in `agent/src/pool.ts`).
pub const EVERY: Duration = Duration::from_secs(60);

/// The inputs every pass shares.
pub struct Links {
    pub backlog_url: String,
    pub access: Arc<dyn AccessTokens>,
    pub paused: PauseState,
    /// The store's pause for each project, recorded from the pass's read (`obey-shared-pause`).
    pub store_paused: StorePauseState,
    /// Item commands (Continue) queued by the connector's thread, drained on each pass.
    pub item_commands: ItemCommands,
    /// The cloud link's `work_available` wake-ups, one per project.
    pub work: crate::work_signal::WorkSignals,
    /// This computer's name and the host of the retaskable.com it links to, as `health` and a claim by name are told them.
    pub machine: String,
    pub environment: String,
    /// This computer's tags, as `health` and a claim by name are told them (agent-settings.md section 3.1).
    pub tags: crate::tags::LiveTags,
    /// Read for `RETASKABLE_CLAUDE`/`PATH` when a playtest grouping round resolves its Claude binary.
    pub env: Env,
    /// Which engine a playtest grouping round's `Role::Planner` session runs on.
    pub engine_choice: EngineChoiceConfig,
    /// The Cursor model names a playtest grouping round's planner session uses on Cursor.
    pub cursor_models: CursorModels,
    /// The latest setup checks, for the engines `run_take` says this computer has. None have run yet: none are sent.
    pub checks: SetupChecksHandle,
    /// Each project's hub link, for reporting Retaskable's own bugs (06.9 bullet 19). A project without one reports none.
    pub bugs: std::collections::HashMap<String, Arc<crate::connector::bug_link::BugLink>>,
    /// What a bug report says the build is.
    pub bug_version: String,
}

/// `links`, the computer-wide own failure and gates ([`Machine::own_failure`], [`Machine::claude_signed_out_until`]),
/// borrowed together: what a project's claim and start methods need from the [`Machine`] besides the lane
/// pool, bundled so threading them through doesn't grow those methods past clippy's argument limit.
pub struct Shared<'a> {
    pub links: &'a Links,
    pub own_failure: &'a mut Option<crate::backlog::pool::OwnFailure>,
    pub claude_signed_out_until: &'a mut Option<Instant>,
    /// Which engines are at their usage limit on this computer ([`Machine::engine_limits`]).
    pub engine_limits: &'a mut EngineLimits,
    /// The local model, for the limit fallback ([`StartGate`]).
    pub local: &'a crate::local_model::LocalModel,
    /// [`process_tree::stopping`] as of this pass's start: a restart or Quit cutting every
    /// session, so no project claims or starts new work either
    /// (docs/specs/agent/07.4-stopping-and-restarting.md, `restart-cuts-sessions`).
    pub stopping: bool,
}

/// The computer's orchestrator: one shared lane pool and every project's state. Each pass reads every
/// project's backlog (in parallel, each in its own task), then the projects take turns deciding starts
/// on the shared lanes (`round_robin::plan`), as the TS agent's `Pool.tick` does.
pub struct Machine {
    pub(super) pool: LanePool,
    /// Set by an own failure before any session ran ([`super::settle::hand_back`]) and reported in every project's `health`
    /// until a session starts well (`settle`). The store holds the pause (`wait`); this computer has none of its own
    /// (pools-on-server.md section 14.4). Cleared by a restart.
    pub(super) own_failure: Option<crate::backlog::pool::OwnFailure>,
    /// Set when a session ends having found Claude Code signed out ([`super::settle::settle`]): no new or
    /// resumed Claude session starts on this computer until this time, the TS agent's `claudeSignedOut` gate
    /// (section 7.3). Nothing but time clears it yet, so each pass just retries after it passes.
    pub(super) claude_signed_out_until: Option<Instant>,
    /// Which engines are at their usage limit and until when, from a session's `limit_hit`: a resume of a
    /// session on one starts fresh on another engine that can take it (engines/03 section 3.5). In memory only.
    pub(super) engine_limits: EngineLimits,
    /// The pace of the usage recheck while Claude's limit holds the computer.
    pub(super) usage_check: crate::usage_check::UsageCheck,
    pub(super) projects: Vec<ProjectLoop>,
    /// Index in `projects` of the project served last: the next start goes to the one after it.
    pub(super) last_served: Option<usize>,
    pub(super) links: Links,
    /// Put in front of a project's id to key its base checkout hold; empty in the agent. Tests give each
    /// machine its own, since the hold is shared by the whole process and every test rig names a project `p0`.
    pub(super) base_scope: String,
    /// Runs the sessions for what is claimed. With none, `claim` mode runs dry.
    pub(super) starter: Option<Arc<dyn SessionStarter>>,
    /// Runs spec drafts. With none, no draft is claimed.
    pub(super) drafts: Option<Arc<dyn super::DraftHost>>,
    /// Runs the jobs the store sends as tasks. With none, `health` says it can run none and every job is refused.
    pub(super) jobs: Option<Arc<dyn crate::jobs::host::JobHost>>,
    /// Answers the chat's asks. With none, asks are left unanswered.
    pub(super) asks: Option<Arc<dyn super::AskHost>>,
    /// Whether a person's take-over requests are pulled. Off unless a session starter is wired.
    pub(super) take_overs: bool,
    /// Whether the site's Add spec and Build this rows (`draft_wanted`) are pulled. Off unless wired.
    pub(super) wanted: bool,
    /// Finishes items a person marks Done. With none, `finish_requested` rows are left alone.
    pub(super) finish_host: Option<Arc<dyn super::finish_steps::FinishHost>>,
    /// Merges items a person presses Merge into main on. With none, `merge_requested` rows are left alone.
    pub(super) merge_host: Option<Arc<dyn super::merge_item::MergeHost>>,
    /// Opens the pull requests a person presses Open PR to main for. With none, `main_pr_requested` rows are left alone.
    pub(super) main_pr_host: Option<Arc<dyn super::main_pr::MainPrHost>>,
    /// Watches the open pull requests, report only. With none, no pass runs.
    pub(super) ci_watch: Option<Arc<dyn super::CiWatchHost>>,
    /// Which projects this computer is the rollout runner of: only there does the CI watcher look.
    pub(super) runners: crate::rollout::Runners,
    /// The rows that start work this computer is running, over every project (at most two at once).
    pub(super) work: crate::intents::Work,
    /// Runs the expert sessions' turns. With none, the expert commands are refused.
    pub(super) experts: Option<Arc<dyn super::ExpertHost>>,
    /// The wait between merge tries after a draft is rebased.
    pub(super) merge_wait: std::time::Duration,
    /// Does the work of runs in `runs` mode. With none, that mode drops old runs and asks for nothing.
    pub(super) run_starters: RunStarters,
    /// How many sessions may run at once, across every project (`concurrency`).
    pub(super) concurrency: u32,
    /// The computer's local model: its slot holder doesn't count toward `concurrency`.
    pub(super) local: crate::local_model::LocalModel,
    /// Sessions report their end here; the receiver is read on each wake.
    pub(super) outcomes: mpsc::UnboundedReceiver<super::SessionOutcome>,
    pub(super) outcome_sender: OutcomeSender,
    /// Stands in for [`process_tree::stopping`] in a test, which can't set that process-wide
    /// flag itself without also exposing every other test in the binary to it. `None` outside
    /// tests, always: production always reads the real flag.
    #[cfg(test)]
    pub(super) stopping_override: Option<bool>,
}

/// A [`Shared`] borrowing the machine's fields (not its projects), so a loop over `self.projects` can build one.
macro_rules! shared {
    ($m:ident, $stopping:expr) => {
        Shared {
            links: &$m.links,
            local: &$m.local,
            own_failure: &mut $m.own_failure,
            claude_signed_out_until: &mut $m.claude_signed_out_until,
            engine_limits: &mut $m.engine_limits,
            stopping: $stopping,
        }
    };
}

impl Links {
    /// The access token every store call carries: `None` when this computer isn't linked to the store, an error when
    /// the store couldn't be reached to get one.
    pub async fn store_token(&self) -> anyhow::Result<Option<String>> {
        crate::backlog::access::current(&*self.access).await
    }
}

impl Machine {
    pub fn new(lane_count: u32, concurrency: u32, links: Links) -> Self {
        let (sender, outcomes) = mpsc::unbounded_channel();
        Self {
            pool: LanePool::new(PoolRules {
                lanes: lane_count,
                ..Default::default()
            }),
            own_failure: None,
            claude_signed_out_until: None,
            engine_limits: EngineLimits::default(),
            usage_check: Default::default(),
            projects: Vec::new(),
            last_served: None,
            links,
            base_scope: String::new(),
            starter: None,
            drafts: None,
            jobs: None,
            asks: None,
            take_overs: false,
            wanted: false,
            finish_host: None,
            merge_host: None,
            main_pr_host: None,
            ci_watch: None,
            runners: Default::default(),
            work: Default::default(),
            experts: None,
            merge_wait: super::land::MERGE_WAIT,
            run_starters: RunStarters::default(),
            concurrency,
            local: crate::local_model::LocalModel::off(),
            outcomes,
            outcome_sender: OutcomeSender(sender),
            #[cfg(test)]
            stopping_override: None,
        }
    }

    /// Shares the computer's local model, so the session holding its slot can be left out of the count.
    pub fn with_local(mut self, local: crate::local_model::LocalModel) -> Self {
        self.local = local;
        self
    }

    /// `concurrency` plus the session holding the local slot (already in the starter's counts). Applied once, to
    /// the base number, so it can't be added back twice.
    fn room_total(&self, concurrency: u32) -> u32 {
        concurrency + self.local.holding()
    }

    /// [`process_tree::stopping`], unless a test has overridden it.
    pub(super) fn stopping(&self) -> bool {
        #[cfg(test)]
        if let Some(stopping) = self.stopping_override {
            return stopping;
        }
        process_tree::stopping()
    }

    /// The sender every started session gets (inside its [`super::StartRequest`]).
    pub fn outcome_sender(&self) -> OutcomeSender {
        self.outcome_sender.clone()
    }

    /// Tells each project whether this computer takes the store's tasks (it has a session runner and the mode acts), which
    /// decides what it holds, ships and asks the store for.
    pub(super) fn mark_task_takers(&mut self) {
        let takes: Vec<bool> = self.projects.iter().map(|lp| self.claims(lp)).collect();
        for (lp, takes) in self.projects.iter_mut().zip(takes) {
            lp.takes_tasks = takes;
        }
    }

    /// Whether a project's loop runs the item machinery (the store's tasks): `claim` mode with a starter, or `runs` mode
    /// with one, which takes tasks beside the pieces it takes by `run_take` (the hybrid, as the TypeScript agent runs).
    pub(super) fn claims(&self, lp: &ProjectLoop) -> bool {
        self.starter.is_some() && matches!(lp.mode, OrchestratorMode::Claim | OrchestratorMode::Runs)
    }

    /// Whether a project's loop acts at all: [`Self::claims`], or `runs` mode with a runner to take runs. The passes
    /// that aren't about an item (asks, ideas) run wherever this holds.
    pub(super) fn acts(&self, lp: &ProjectLoop) -> bool {
        self.claims(lp) || (lp.mode == OrchestratorMode::Runs && !self.run_starters.0.is_empty())
    }

    /// Whether each project holds new work: this computer's pause is on, or the
    /// store's is on or unknown (fails closed; work already running carries on).
    fn paused(&self) -> impl std::future::Future<Output = Vec<bool>> + Send + 'static {
        let projects: Vec<_> = (self.projects.iter())
            .map(|lp| {
                let own = self.links.paused.reason(&lp.id).is_some();
                (own, self.links.store_paused.get(&lp.id))
            })
            .collect();
        async move {
            let mut out = Vec::new();
            for (own, store) in projects {
                out.push(own || store != StorePause::NotPaused);
            }
            out
        }
    }

    /// The poll between the minute's passes (`pollIntervalSec`): the live projects take in their ended steps,
    /// advance their items and start what the free lanes and session slots allow, without reading the backlog
    /// or writing a `tick`. Does nothing with no starter. A panic costs this poll only.
    pub async fn poll(&mut self) {
        let ready: Vec<bool> = self.projects.iter().map(|lp| self.claims(lp)).collect();
        if !ready.contains(&true) {
            return;
        }
        let paused = self.paused().await;
        if let Err(panic) = AssertUnwindSafe(self.schedule(&ready, &paused)).catch_unwind().await {
            tracing::error!("orchestrator: a poll panicked, skipping it: {}", panic_message(&*panic));
        }
    }

    /// The minute's `git fetch` of every live project's repo, paused or not (section 7: the fetch runs while
    /// paused too), each in parallel, and then what it saw told to the store (`report`). A failed fetch is
    /// logged and an `error` event; the loop goes on.
    pub async fn fetch(&mut self) {
        self.fetch_for(None).await;
    }

    /// The put-off syncs that are now due (the base checkout was given back): fetched on this poll, not the
    /// next minute. A sync that finds the base still held is put off again.
    pub async fn fetch_due(&mut self) {
        let due = base_hold::take_due_syncs(&self.base_scope);
        if !due.is_empty() {
            self.fetch_for(Some(&due)).await;
        }
    }

    /// Each project's fetch holds its base checkout ("a sync") for the length of the fetch; when that's held
    /// the project's sync is put off (no fetch, no error event) and runs on the poll after it's given back.
    async fn fetch_for(&mut self, only: Option<&std::collections::HashSet<String>>) {
        let jobs: Vec<_> = (self.projects.iter())
            .filter(|lp| self.claims(lp) && only.is_none_or(|o| o.contains(&lp.id)))
            .filter_map(|lp| {
                let hold = base_hold::hold_base_for_sync(&format!("{}{}", self.base_scope, lp.id))?;
                let repo = std::path::PathBuf::from(&lp.project.repo_path);
                Some((
                    lp.id.clone(),
                    tokio::task::spawn_blocking(move || {
                        let _hold = hold;
                        lanes::fetch_latest(&repo)
                    }),
                ))
            })
            .collect();
        for (id, job) in jobs {
            let Some(lp) = self.projects.iter_mut().find(|lp| lp.id == id) else {
                continue;
            };
            let failed = match job.await {
                Ok(Ok(_)) => {
                    let before = lp.seen.main.clone();
                    lp.report(&self.links).await;
                    // Main moved: the project's rollout loop makes its report now, not when its wait is up.
                    if lp.seen.main != before {
                        self.runners.wake(&id).notify_one();
                    }
                    continue;
                }
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => e.to_string(),
            };
            tracing::warn!("{id}: git fetch failed: {failed}");
            let error = event::Error {
                r#where: Some("fetch".into()),
                error: Some(failed),
                ..Default::default()
            };
            lp.events.add_in(&id, Event::Error(error));
        }
    }

    /// Gives `runs` mode the starters of the kinds it can run.
    pub fn with_run_starters(mut self, starters: Vec<Arc<dyn RunStarter>>) -> Self {
        self.run_starters = RunStarters(starters);
        self
    }

    /// One pass of the run loop for every project in `runs` mode. A paused project or a stopping computer
    /// asks for nothing but still drops, ends and beats. A panic costs this pass only.
    pub async fn run_passes(&mut self) {
        self.mark_task_takers();
        let stopping = self.stopping() || crate::runs::winding_down();
        let paused = self.paused().await;
        for (lp, paused) in self.projects.iter_mut().zip(paused.iter().copied()) {
            if lp.mode != OrchestratorMode::Runs {
                continue;
            }
            let mut shared = Shared {
                links: &self.links,
                local: &self.local,
                own_failure: &mut self.own_failure,
                claude_signed_out_until: &mut self.claude_signed_out_until,
                engine_limits: &mut self.engine_limits,
                stopping: stopping || paused,
            };
            let pass = lp.run_pass(&mut self.pool, &self.run_starters, &shared, Instant::now());
            if let Err(panic) = AssertUnwindSafe(pass).catch_unwind().await {
                tracing::error!(
                    "{}: a run pass panicked, skipping it: {}",
                    lp.id,
                    panic_message(&*panic)
                );
            }
            // A person's Sync now: no session and no slot, and a pause doesn't hold it back. With no runner, `runs`
            // mode asks for nothing.
            if !self.run_starters.0.is_empty() {
                shared.stopping = stopping;
                lp.sync_now_pass(&shared).await;
            }
        }
        // The passes that aren't about an item: asks, experts, a Build this, and drafts.
        let on: Vec<bool> = (self.projects.iter())
            .map(|lp| lp.mode == OrchestratorMode::Runs && self.acts(lp) && !self.claims(lp))
            .collect();
        let mut slots = self.room_total(self.concurrency).saturating_sub(self.sessions());
        self.ask_passes(&on, &paused, stopping, &mut slots).await;
        self.merge_passes(&on, stopping).await;
        self.main_pr_passes(&on, stopping).await;
        self.expert_passes(&on, stopping, &mut slots).await;
        let watching: Vec<bool> = (self.projects.iter().zip(&on))
            .map(|(lp, &on)| on && (self.claims(lp) || lp.backlog_items.is_some()))
            .collect();
        self.ci_passes(&watching, &paused, stopping).await;
        self.wanted_passes(&on, &paused, stopping).await;
        self.draft_passes(&on, &paused, stopping, &mut slots).await;
    }

    /// Lets `claim` mode claim and start work.
    pub fn with_starter(mut self, starter: Arc<dyn SessionStarter>) -> Self {
        self.starter = Some(starter);
        self
    }

    /// Lets `claim` mode take over items a person asks for.
    pub fn with_take_overs(mut self) -> Self {
        self.take_overs = true;
        self
    }

    /// Lets `claim` mode finish items a person marks Done.
    pub fn with_finish_host(mut self, host: Arc<dyn super::finish_steps::FinishHost>) -> Self {
        self.finish_host = Some(host);
        self
    }

    /// Lets `claim` and `runs` mode merge what a person presses Merge into main on (an item only in `claim` mode).
    pub fn with_merge_host(mut self, host: Arc<dyn super::merge_item::MergeHost>) -> Self {
        self.merge_host = Some(host);
        self
    }

    /// Lets `claim` and `runs` mode open the pull request into main a person asks for.
    pub fn with_main_pr_host(mut self, host: Arc<dyn super::main_pr::MainPrHost>) -> Self {
        self.main_pr_host = Some(host);
        self
    }

    /// Lets `claim` and `runs` mode watch the open pull requests (report only) of the projects `runners` says this
    /// computer runs.
    pub fn with_ci_watch(mut self, host: Arc<dyn super::CiWatchHost>, runners: crate::rollout::Runners) -> Self {
        self.ci_watch = Some(host);
        self.runners = runners;
        self
    }

    /// Lets `claim` and `runs` mode run expert sessions.
    pub fn with_experts(mut self, host: Arc<dyn super::ExpertHost>) -> Self {
        self.experts = Some(host);
        self
    }

    /// Lets `claim` and `runs` mode answer the chat's asks.
    pub fn with_asks(mut self, host: Arc<dyn super::AskHost>) -> Self {
        self.asks = Some(host);
        self
    }

    /// Lets `claim` and `runs` mode take the site's Add spec and Build this rows, queueing each as a draft in the store.
    pub fn with_draft_wanted(mut self) -> Self {
        self.wanted = true;
        self
    }

    /// Lets the computer run the jobs the store sends (`task.job`) and say what it `can` run.
    pub fn with_job_host(mut self, host: Arc<dyn crate::jobs::host::JobHost>) -> Self {
        self.jobs = Some(host);
        self
    }

    /// Lets `claim` and `runs` mode claim and run spec drafts.
    pub fn with_drafts(mut self, host: Arc<dyn super::DraftHost>) -> Self {
        self.drafts = Some(host);
        self
    }

    pub fn add(&mut self, mut project: ProjectLoop) {
        project.outcomes = self.outcome_sender.clone();
        self.projects.push(project);
    }

    /// One pass over every project, in the order added. A project whose read fails (or panics) is skipped
    /// with its own `store_error` or live-state failure; the others go on.
    pub async fn pass(&mut self) -> Vec<Step> {
        self.write_host_calls();
        self.projects.iter_mut().for_each(ProjectLoop::write_session_events);
        let jobs: Vec<_> = self
            .projects
            .iter()
            .map(|lp| {
                tokio::spawn(read(
                    lp.client.clone(),
                    lp.id.clone(),
                    lp.project.github_repo.clone(),
                    lp.repo,
                    self.links.access.clone(),
                ))
            })
            .collect();
        let mut read_items = Vec::new();
        for (lp, job) in self.projects.iter_mut().zip(jobs) {
            let done = job.await.unwrap_or_else(|e| {
                tracing::error!("{}: the backlog read failed: {e}", lp.id);
                Read {
                    repo: lp.repo,
                    items: Err(Fail::Cant(format!("the backlog read failed: {e}"))),
                    settings: None,
                    facts: None,
                    shown: None,
                }
            });
            lp.repo = done.repo.or(lp.repo);
            if let Some(id) = done.repo
                && lp.store.set_repo_id(&lp.project.github_repo, id)
                && let Err(e) = lp.store.save()
            {
                tracing::warn!("{}: couldn't save the repo id: {e:#}", lp.id);
            }
            lp.live.set_repo_id(&lp.id, done.repo);
            // A failed read, of the items or the facts, is unknown.
            let pause = match (&done.items, done.facts) {
                (Ok(_), Some(Ok(facts))) if facts.paused => {
                    StorePause::Paused(facts.paused_reason.unwrap_or_else(|| "Paused for everyone".to_string()))
                }
                (Ok(_), Some(Ok(_))) => StorePause::NotPaused,
                _ => StorePause::Unknown,
            };
            self.links.store_paused.set(&lp.id, pause);
            if let (Ok(_), Some(Ok(shown))) = (&done.items, done.shown) {
                lp.live.set_shown(&lp.id, shown);
            }
            lp.take_allowed_commands(&self.links.item_commands, done.settings);
            lp.report_settings(
                &self.links.item_commands,
                &*self.links.access,
                self.links.machine.as_str(),
            )
            .await;
            read_items.push(match done.items {
                Ok(items) => Some(items),
                Err(Fail::Cant(message)) => {
                    lp.cant_start(message);
                    None
                }
                Err(Fail::Store(e)) => {
                    lp.store_error("list", &e);
                    None
                }
            });
        }
        let paused = self.paused().await;
        let n = self.projects.len();
        // A panic deciding or recording costs this pass only: it is logged and the next pass runs.
        match AssertUnwindSafe(self.decide_and_record(read_items, paused))
            .catch_unwind()
            .await
        {
            Ok(steps) => steps,
            Err(panic) => {
                tracing::error!("orchestrator: a pass panicked, skipping it: {}", panic_message(&*panic));
                vec![Step::Skipped; n]
            }
        }
    }

    /// The pass after the reads: takes back held leases, schedules, then records each project's pass.
    async fn decide_and_record(&mut self, read_items: Vec<Option<Vec<Item>>>, paused: Vec<bool>) -> Vec<Step> {
        self.mark_task_takers();
        for (i, items) in read_items.iter().enumerate() {
            let Some(items) = items else { continue };
            if self.acts(&self.projects[i]) {
                self.projects[i].backlog_items = Some(items.len());
            }
            // The whole board's branches in both modes (TS reads the whole board too): an item another computer holds is
            // not this one's to judge, fix or merge main into.
            self.projects[i].listed_branches = items.iter().flat_map(Item::branches).map(String::from).collect();
            if self.claims(&self.projects[i]) {
                let machine = self.links.machine.clone();
                self.projects[i].readopt(items, &mut self.pool, machine.as_str());
                // Held in the store by this computer with no lease here (a restart dropped it): its own lease back.
                self.projects[i].take_back_own(items, &shared!(self, false)).await;
                // A person's answer kept on an item parked for it, from the site or the MCP: applied as an `answer` command is.
                self.projects[i].take_site_answers(items, &self.links).await;
            }
        }
        if self.starter.is_some() {
            self.flush_all_owed().await;
        }
        let ready: Vec<bool> = read_items.iter().map(Option::is_some).collect();
        let scheduled = self.schedule(&ready, &paused).await;
        let mut steps = Vec::new();
        for (((lp, items), paused), (actions, live)) in
            (self.projects.iter_mut().zip(read_items).zip(paused)).zip(scheduled)
        {
            let cant_start = lp.start_blocked(&shared!(self, false), &self.pool);
            lp.report_limit(cant_start, &self.engine_limits);
            match items {
                Some(items) => steps.push(lp.record(items, paused, cant_start, actions, &self.pool, live).await),
                None => steps.push(Step::Skipped),
            }
        }
        steps
    }

    /// One step of the loop for the `ready` projects (read this pass, or live on a poll). The sessions that ended
    /// are settled, and the live projects take in their ended steps (paused or not) and, unless paused, advance
    /// their items. Then starts are planned round robin within the
    /// session slots left (`concurrency` less the starter's `active`), and the live ones apply them and ask the
    /// store for the lanes and slots still free. Each project's actions, and for a live one its claim answer.
    async fn schedule(&mut self, ready: &[bool], paused: &[bool]) -> Vec<(Vec<Action>, Option<Option<String>>)> {
        self.mark_task_takers();
        self.recheck_usage().await;
        for (lp, ready) in self.projects.iter_mut().zip(ready) {
            // The cutoff is the store's `weekCutoffPct`: the saved reading is read against it once it is known.
            if *ready && !lp.week_checked && lp.has_store_numbers() {
                lp.week_checked = true;
                for usage in [lp.store.usage(), lp.store.week_usage()].into_iter().flatten() {
                    ProjectLoop::cut_off_for_week(&lp.id, &usage, &mut self.engine_limits);
                }
            }
        }
        let stopping = self.stopping();
        let starter = self.starter.clone();
        let live: Vec<bool> = (self.projects.iter().zip(ready))
            .map(|(lp, &ready)| ready && self.claims(lp))
            .collect();
        for (lp, &live) in self.projects.iter_mut().zip(&live) {
            if live {
                lp.load_decisions(&shared!(self, stopping)).await;
                lp.send_outbox(&shared!(self, stopping)).await;
            }
        }
        if starter.is_some() {
            self.settle_outcomes().await;
        }
        let concurrency = self.room_total(self.concurrency);
        if let Some(starter) = starter.as_deref() {
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live {
                    if !paused && !stopping {
                        lp.release_silenced(concurrency.saturating_sub(starter.active()));
                    }
                    // What starts, resumes or steps on the store's numbers waits for them (`has_store_numbers`).
                    if lp.has_store_numbers() {
                        lp.resume_sessions(&mut self.pool, starter, &mut shared!(self, stopping))
                            .await;
                    }
                    lp.take_finished(&mut self.pool, &self.links).await;
                    lp.hear_site_answers(&self.links).await;
                    lp.claim_back(&self.links).await;
                    lp.hear_tryout_problems(&self.links).await;
                    lp.merge_turns(&self.links).await;
                    if stopping {
                        lp.cut_steps();
                    }
                    lp.carry_out_lane_commands(
                        &mut self.pool,
                        &|dir: &std::path::Path, command: &str, lane: u32| starter.play_lane(dir, command, lane),
                        &self.links,
                        paused || stopping,
                    );
                    lp.carry_out_visits(&self.links, paused || stopping).await;
                    if !paused {
                        let room = concurrency.saturating_sub(starter.active());
                        lp.carry_out_commands(&mut self.pool, starter, room, &mut shared!(self, stopping))
                            .await;
                        if lp.has_store_numbers() {
                            lp.advance(&mut self.pool, starter);
                        }
                    }
                    lp.flush_decisions(&shared!(self, stopping)).await;
                }
            }
        }
        let mut slots = concurrency.saturating_sub(self.sessions());
        self.ask_passes(&live, paused, stopping, &mut slots).await;
        // A person taking over an item whose owner's computer is gone: no session, so no slot.
        for (lp, &live) in self.projects.iter_mut().zip(&live) {
            if live && self.take_overs {
                let shared = shared!(self, stopping);
                lp.take_over_pass(&shared).await;
            }
        }
        // A person's Done on an item that needs help here: no session, so no slot.
        if let Some(host) = self.finish_host.clone() {
            for (lp, &live) in self.projects.iter_mut().zip(&live) {
                if live {
                    let shared = shared!(self, stopping);
                    lp.finish_pass(&host, &shared).await;
                }
            }
        }
        self.merge_passes(&live, stopping).await;
        self.main_pr_passes(&live, stopping).await;
        self.ci_passes(&live, paused, stopping).await;
        self.expert_passes(&live, stopping, &mut slots).await;
        // A person's Add spec or Build this: a `draft_wanted` row, queued as a draft in the store ahead of any new work.
        self.wanted_passes(&live, paused, stopping).await;
        self.draft_passes(&live, paused, stopping, &mut slots).await;
        let mut candidates: Vec<Option<Candidate>> = (self.projects.iter().zip(ready).zip(paused))
            .map(|((lp, &ready), &paused)| {
                ready.then(|| Candidate {
                    id: &lp.id,
                    paused,
                    runs: lp.runs.clone(),
                    cfg: &lp.project,
                })
            })
            .collect();
        let (plans, last) = plan(&self.pool, self.last_served, slots, &mut candidates);
        drop(candidates);
        self.last_served = last;
        let mut scheduled = Vec::new();
        let job_host = self.jobs.clone();
        // Claude implementer starts this pass, over every project: the model slots are shared by the computer.
        let mut claude_starts = 0;
        for (((lp, actions), live), &paused) in (self.projects.iter_mut().zip(plans))
            .zip(live.iter().copied())
            .zip(paused)
        {
            let mut answer = None;
            lp.claude_starts = claude_starts;
            if let Some(starter) = starter.as_deref().filter(|_| live) {
                if lp.has_store_numbers() {
                    slots = slots.saturating_sub(
                        lp.apply_starts(&actions, &mut self.pool, starter, &mut shared!(self, stopping))
                            .await,
                    );
                }
                // The store sends the work: a report each minute, and the tasks it answers started.
                answer = Some(
                    lp.pool_pass(
                        paused,
                        &mut self.pool,
                        &mut slots,
                        starter,
                        job_host.as_ref(),
                        &mut shared!(self, stopping),
                    )
                    .await,
                );
            }
            claude_starts = lp.claude_starts;
            scheduled.push((actions, answer));
        }
        scheduled
    }
}

/// The latest setup checks (`runtime::spawn_setup_checks`).
pub type SetupChecksHandle = Arc<std::sync::Mutex<Option<crate::setup_check::SetupChecks>>>;
