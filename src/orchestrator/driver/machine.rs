//! The computer's orchestrator: one shared lane pool and every project's state, and what starts the
//! whole loop running (`spawn_all`).
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use super::panic::panic_message;
use super::read::{Fail, Read, read};
use super::run_starters::RunStarters;
use super::{
    OrchestratorMode, OutcomeSender, ProjectLoop, RunStarter, SessionStarter, Step, deprecations, ideas_daily,
    playtest_round, vet_daily,
};
use crate::backlog::{Client, Item};
use crate::config::{AutoConcurrency, CursorModels, EngineChoiceConfig, Playtests, ProjectConfig};
use crate::env::Env;
use crate::lane_pool::{LanePool, PoolRules};
use crate::lanes;
use crate::link::TokenStore;
use crate::orchestrator::Action;
use crate::orchestrator::fresh_start::EngineLimits;
use crate::orchestrator::guard::PortGuard;
use crate::orchestrator::live::LiveHandle;
use crate::orchestrator::round_robin::{Candidate, plan};
use crate::process_tree;
use crate::telemetry::{Event, event};
use crate::view::{ItemCommands, PauseState};

/// How often the loop reads every project's backlog and writes its `tick` events: the TS agent's once a
/// minute (`MINUTE_MS` in `agent/src/pool.ts`).
pub const EVERY: Duration = Duration::from_secs(60);

/// GitHub's numeric repo id (`gh api repos/<repo> --jq .id`, `repoId` in `agent/src/gh.ts`), which names the
/// project's backlog in the store.
pub(crate) async fn repo_id(github_repo: &str) -> Option<u64> {
    let out = tokio::process::Command::new("gh")
        .args(["api", &format!("repos/{github_repo}"), "--jq", ".id"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

/// The inputs every pass shares.
pub struct Links {
    pub backlog_url: String,
    pub keychain: Arc<dyn TokenStore>,
    pub paused: PauseState,
    /// Item commands (Continue) queued by the connector's thread, drained on each pass.
    pub item_commands: ItemCommands,
    /// The cloud link's `work_available` wake-ups, one per project.
    pub work: crate::work_signal::WorkSignals,
    /// This computer's name and the host of the retaskable.com it links to, as `claim_next` is told them.
    pub machine: String,
    pub environment: String,
    /// This computer's tags, as `claim_next` is told them (agent-settings.md section 3.1).
    pub tags: Vec<String>,
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

/// `links` and the computer-wide gates ([`Machine::claim_paused_until`], [`Machine::claude_signed_out_until`]),
/// borrowed together: what a project's claim and start methods need from the [`Machine`] besides the lane
/// pool, bundled so threading them through doesn't grow those methods past clippy's argument limit.
pub struct Shared<'a> {
    pub links: &'a Links,
    pub claim_paused_until: &'a mut Option<Instant>,
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
    /// Set by an own failure before any session ran ([`super::settle::hand_back`]): no `claim_next` for any
    /// project on this computer until this time (the TS agent's 30-minute claim pause, section 7.3). Cleared
    /// by a restart.
    pub(super) claim_paused_until: Option<Instant>,
    /// Set when a session ends having found Claude Code signed out ([`super::settle::settle`]): no new or
    /// resumed Claude session starts on this computer until this time, the TS agent's `claudeSignedOut` gate
    /// (section 7.3). Nothing but time clears it yet, so each pass just retries after it passes.
    pub(super) claude_signed_out_until: Option<Instant>,
    /// Which engines are at their usage limit and until when, from a session's `limit_hit`: a resume of a
    /// session on one starts fresh on another engine that can take it (engines/03 section 3.5). In memory only.
    pub(super) engine_limits: EngineLimits,
    pub(super) projects: Vec<ProjectLoop>,
    /// Index in `projects` of the project served last: the next start goes to the one after it.
    pub(super) last_served: Option<usize>,
    pub(super) links: Links,
    /// Runs the sessions for what is claimed. With none, `claim` mode runs dry.
    pub(super) starter: Option<Arc<dyn SessionStarter>>,
    /// Runs spec drafts. With none, no draft is claimed.
    pub(super) drafts: Option<Arc<dyn super::DraftHost>>,
    /// Runs the trial plan, with the latest setup checks. With none, no trial starts.
    pub(super) trial: Option<(Arc<dyn crate::orchestrator::trial_host::TrialHost>, SetupChecksHandle)>,
    /// Answers the chat's asks. With none, asks are left unanswered.
    pub(super) asks: Option<Arc<dyn super::AskHost>>,
    /// The wait between merge tries after a draft is rebased.
    pub(super) merge_wait: std::time::Duration,
    /// Does the work of runs in `runs` mode. With none, that mode drops old runs and asks for nothing.
    pub(super) run_starters: RunStarters,
    /// How many sessions may run at once, across every project (`concurrency`).
    pub(super) concurrency: u32,
    /// The computer's local model: its slot holder doesn't count toward `concurrency` (`local-one-slot`).
    pub(super) local: crate::local_model::LocalModel,
    /// Auto concurrency's pacer and the number it allows (`driver/auto.rs`).
    pub(super) auto: super::auto::Auto,
    /// Sessions report their end here; the receiver is read on each wake.
    pub(super) outcomes: mpsc::UnboundedReceiver<super::SessionOutcome>,
    pub(super) outcome_sender: OutcomeSender,
    /// Stands in for [`process_tree::stopping`] in a test, which can't set that process-wide
    /// flag itself without also exposing every other test in the binary to it. `None` outside
    /// tests, always: production always reads the real flag.
    #[cfg(test)]
    pub(super) stopping_override: Option<bool>,
    /// A test's clock, in epoch seconds, for the pacer.
    #[cfg(test)]
    pub(super) clock: Option<i64>,
}

/// A [`Shared`] borrowing the machine's fields (not its projects), so a loop over `self.projects` can build one.
macro_rules! shared {
    ($m:ident, $stopping:expr) => {
        Shared {
            links: &$m.links,
            local: &$m.local,
            claim_paused_until: &mut $m.claim_paused_until,
            claude_signed_out_until: &mut $m.claude_signed_out_until,
            engine_limits: &mut $m.engine_limits,
            stopping: $stopping,
        }
    };
}

impl Machine {
    pub fn new(lane_count: u32, concurrency: u32, links: Links) -> Self {
        let (sender, outcomes) = mpsc::unbounded_channel();
        Self {
            pool: LanePool::new(PoolRules {
                lanes: lane_count,
                ..Default::default()
            }),
            claim_paused_until: None,
            claude_signed_out_until: None,
            engine_limits: EngineLimits::default(),
            projects: Vec::new(),
            last_served: None,
            links,
            starter: None,
            drafts: None,
            trial: None,
            asks: None,
            merge_wait: super::land::MERGE_WAIT,
            run_starters: RunStarters::default(),
            concurrency,
            local: crate::local_model::LocalModel::off(),
            auto: super::auto::Auto::new(AutoConcurrency {
                enabled: false,
                ..Default::default()
            }),
            outcomes,
            outcome_sender: OutcomeSender(sender),
            #[cfg(test)]
            stopping_override: None,
            #[cfg(test)]
            clock: None,
        }
    }

    /// Shares the computer's local model, so the session holding its slot can be left out of the count.
    pub fn with_local(mut self, local: crate::local_model::LocalModel) -> Self {
        self.local = local;
        self
    }

    /// `concurrency` plus the session holding the local slot (already in the starter's counts), when `local-one-slot`
    /// is On. Applied once, to the base number, so it can't be added back twice.
    fn room_total(&self, concurrency: u32) -> u32 {
        if crate::tiers::local_one_slot_on() {
            concurrency + self.local.holding()
        } else {
            concurrency
        }
    }

    /// Paces sessions with auto concurrency (behind the `auto-concurrency` flag) when `cfg.enabled`.
    pub fn with_auto(mut self, cfg: AutoConcurrency) -> Self {
        self.auto = super::auto::Auto::new(cfg);
        self
    }

    /// Keeps the pacer's number across a restart in `<dir>/machine.json`.
    pub fn with_machine_dir(mut self, dir: &Path) -> Self {
        self.auto.dir = Some(dir.to_path_buf());
        self
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

    /// Whether a project's loop really acts: `claim` mode with a starter.
    fn live(&self, lp: &ProjectLoop) -> bool {
        self.starter.is_some() && lp.mode == OrchestratorMode::Claim
    }

    fn paused(&self) -> Vec<bool> {
        (self.projects.iter())
            .map(|lp| self.links.paused.reason(&lp.id).is_some())
            .collect()
    }

    /// The poll between the minute's passes (`pollIntervalSec`): the live projects take in their ended steps,
    /// advance their items and start what the free lanes and session slots allow, without reading the backlog
    /// or writing a `tick`. Does nothing with no starter. A panic costs this poll only.
    pub async fn poll(&mut self) {
        let ready: Vec<bool> = self.projects.iter().map(|lp| self.live(lp)).collect();
        if !ready.contains(&true) {
            return;
        }
        let paused = self.paused();
        if let Err(panic) = AssertUnwindSafe(self.schedule(&ready, &paused)).catch_unwind().await {
            tracing::error!("orchestrator: a poll panicked, skipping it: {}", panic_message(&*panic));
        }
    }

    /// The minute's `git fetch` of every live project's repo, paused or not (section 7: the fetch runs while
    /// paused too), each in parallel, and then what it saw told to the store (`report`). A failed fetch is
    /// logged and an `error` event; the loop goes on.
    pub async fn fetch(&mut self) {
        let jobs: Vec<_> = (self.projects.iter())
            .filter(|lp| self.live(lp))
            .map(|lp| {
                let repo = std::path::PathBuf::from(&lp.project.repo_path);
                (
                    lp.id.clone(),
                    tokio::task::spawn_blocking(move || lanes::fetch_latest(&repo)),
                )
            })
            .collect();
        for (id, job) in jobs {
            let Some(lp) = self.projects.iter_mut().find(|lp| lp.id == id) else {
                continue;
            };
            let failed = match job.await {
                Ok(Ok(_)) => {
                    lp.report(&self.links).await;
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

    /// What a background round (playtest grouping, ideas) needs of project `lp`.
    fn round_job(&self, lp: &ProjectLoop) -> playtest_round::Job {
        playtest_round::Job {
            id: lp.id.clone(),
            client: lp.client.clone(),
            repo: lp.repo,
            project: lp.project.clone(),
            env: self.links.env.clone(),
            engine_choice: self.links.engine_choice.clone(),
            cursor_models: self.links.cursor_models.clone(),
            flags: lp.flags.clone(),
            machine: self.links.machine.clone(),
            backlog_url: self.links.backlog_url.clone(),
            keychain: self.links.keychain.clone(),
            locks: (lp.mode == OrchestratorMode::Runs).then(|| lp.run_loop.runs().locks()),
            sessions: lp.live.sessions().clone(),
        }
    }

    /// Starts the daily round of ideas for every live (or `runs`-mode) project that ticked this minute, isn't
    /// paused and has the `ideas-session` flag On, unless one is still running (`ideas_daily::run` asks the store
    /// whether it's due). Each is its own task, so the pass never waits.
    pub async fn ideas_rounds(&mut self, steps: &[Step]) {
        let paused = self.paused();
        let jobs: Vec<_> = (self.projects.iter().enumerate())
            .filter(|(i, lp)| {
                (self.live(lp) || lp.mode == OrchestratorMode::Runs)
                    && !paused[*i]
                    && matches!(steps.get(*i), Some(Step::Ticked { .. }))
            })
            .map(|(_, lp)| lp)
            .filter(|lp| {
                let busy = lp.ideas_round_running.swap(true, Ordering::SeqCst);
                if busy {
                    tracing::debug!("{}: an ideas round is still running, skipping this minute", lp.id);
                }
                !busy
            })
            .map(|lp| {
                (
                    lp.ideas_round_running.clone(),
                    ideas_daily::Daily {
                        base: self.round_job(lp),
                        flags: lp.flags.clone(),
                        state: lp.ideas_daily.clone(),
                    },
                )
            })
            .collect();
        for (running, daily) in jobs {
            tokio::spawn(async move {
                ideas_daily::run(daily).await;
                running.store(false, Ordering::SeqCst);
            });
        }
    }

    /// Starts the daily deprecated-specs check for every live (or `runs`-mode) project that ticked this minute,
    /// paused or not, unless one is still running. No lane and no session.
    pub async fn deprecation_checks(&mut self, steps: &[Step]) {
        let jobs: Vec<_> = (self.projects.iter().enumerate())
            .filter(|(i, lp)| {
                (self.live(lp) || lp.mode == OrchestratorMode::Runs)
                    && matches!(steps.get(*i), Some(Step::Ticked { .. }))
            })
            .map(|(_, lp)| lp)
            .filter(|lp| !lp.deprecations_running.swap(true, Ordering::SeqCst))
            .map(|lp| {
                (
                    lp.deprecations_running.clone(),
                    deprecations::Daily {
                        base: self.round_job(lp),
                        flags: lp.flags.clone(),
                        state: lp.deprecations.clone(),
                    },
                )
            })
            .collect();
        for (running, daily) in jobs {
            tokio::spawn(async move {
                deprecations::run(daily).await;
                running.store(false, Ordering::SeqCst);
            });
        }
    }

    /// Starts the daily vet pass for every live (or `runs`-mode) project that ticked this minute and isn't paused,
    /// unless one is still running (`vet_daily::run` checks the flag, the setting, the link and `jobs_due`).
    pub async fn vet_rounds(&mut self, steps: &[Step]) {
        for lp in &mut self.projects {
            let record = {
                let mut st = lp.vet_daily.lock().unwrap();
                if !std::mem::take(&mut st.dirty) {
                    continue;
                }
                st.record()
            };
            if let Some(record) = record {
                lp.store.set_vet(&record);
                if let Err(e) = lp.store.save() {
                    tracing::warn!("{}: saving the vet record failed: {e:#}", lp.id);
                }
                lp.live.set_vet(&lp.id, Some(record.counts));
            }
        }
        let paused = self.paused();
        let jobs: Vec<_> = (self.projects.iter().enumerate())
            .filter(|(i, lp)| {
                (self.live(lp) || lp.mode == OrchestratorMode::Runs)
                    && !paused[*i]
                    && matches!(steps.get(*i), Some(Step::Ticked { .. }))
            })
            .map(|(_, lp)| lp)
            .filter(|lp| !lp.vet_running.swap(true, Ordering::SeqCst))
            .map(|lp| {
                (
                    lp.vet_running.clone(),
                    vet_daily::Daily {
                        base: self.round_job(lp),
                        flags: lp.flags.clone(),
                        state: lp.vet_daily.clone(),
                    },
                )
            })
            .collect();
        for (running, daily) in jobs {
            tokio::spawn(async move {
                vet_daily::run(daily).await;
                running.store(false, Ordering::SeqCst);
            });
        }
    }

    /// Runs one playtest-grouping round for every live project that isn't already running one
    /// (`playtest_round::run`), each as its own task so a slow or stuck planner session costs that
    /// project's round only. `steps` is what this minute's [`Machine::pass`] did: a project whose
    /// read failed (`Step::Skipped`), a paused one, one with `playtests: off`, and every project with no
    /// starter or in `dry` mode run no round.
    pub async fn playtest_rounds(&mut self, steps: &[Step]) {
        let paused = self.paused();
        let jobs: Vec<_> = self
            .projects
            .iter()
            .enumerate()
            .filter(|(i, lp)| {
                (self.live(lp) || lp.mode == OrchestratorMode::Runs)
                    && !paused[*i]
                    && matches!(steps.get(*i), Some(Step::Ticked { .. }))
                    && lp.project.playtests != Playtests::Off
            })
            .map(|(_, lp)| lp)
            .filter_map(|lp| {
                if lp.playtest_round_running.swap(true, Ordering::SeqCst) {
                    tracing::debug!(
                        "{}: a playtest grouping round is still running, skipping this one",
                        lp.id
                    );
                    return None;
                }
                Some((lp.playtest_round_running.clone(), self.round_job(lp)))
            })
            .collect();
        for (running, job) in jobs {
            tokio::spawn(async move {
                playtest_round::run(job).await;
                running.store(false, Ordering::SeqCst);
            });
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
        let stopping = self.stopping() || crate::runs::winding_down();
        let paused = self.paused();
        let (mut claim_paused_until, mut signed_out_until) = (None, None);
        let mut limits = EngineLimits::default();
        for (lp, paused) in self.projects.iter_mut().zip(paused) {
            if lp.mode != OrchestratorMode::Runs {
                continue;
            }
            let shared = Shared {
                links: &self.links,
                local: &self.local,
                claim_paused_until: &mut claim_paused_until,
                claude_signed_out_until: &mut signed_out_until,
                engine_limits: &mut limits,
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
        }
    }

    /// Lets `claim` mode claim and start work.
    pub fn with_starter(mut self, starter: Arc<dyn SessionStarter>) -> Self {
        self.starter = Some(starter);
        self
    }

    /// Lets `claim` mode answer the chat's asks.
    pub fn with_asks(mut self, host: Arc<dyn super::AskHost>) -> Self {
        self.asks = Some(host);
        self
    }

    /// Lets `claim` mode claim and run spec drafts.
    pub fn with_drafts(mut self, host: Arc<dyn super::DraftHost>) -> Self {
        self.drafts = Some(host);
        self
    }

    /// Lets `claim` mode run each new project's trial plan, once its setup `checks` pass.
    pub fn with_trial(
        mut self,
        host: Arc<dyn crate::orchestrator::trial_host::TrialHost>,
        checks: SetupChecksHandle,
    ) -> Self {
        self.trial = Some((host, checks));
        self
    }

    pub fn add(&mut self, mut project: ProjectLoop) {
        project.outcomes = self.outcome_sender.clone();
        self.projects.push(project);
    }

    /// One pass over every project, in the order added. A project whose read fails (or panics) is skipped
    /// with its own `store_error` or live-state failure; the others go on.
    pub async fn pass(&mut self) -> Vec<Step> {
        let mut gate_on = Vec::new();
        let asked: Vec<_> = self
            .projects
            .iter()
            .map(|lp| (lp.id.clone(), lp.flags.clone()))
            .collect();
        for (id, flags) in asked {
            let on = flags.on(crate::command_gate::FLAG).await;
            self.links.item_commands.set_flag(&id, crate::command_gate::FLAG, on);
            gate_on.push(on);
        }
        let jobs: Vec<_> = self
            .projects
            .iter()
            .zip(&gate_on)
            .map(|(lp, &on)| {
                tokio::spawn(read(
                    lp.client.clone(),
                    lp.id.clone(),
                    lp.project.github_repo.clone(),
                    lp.repo,
                    self.links.backlog_url.clone(),
                    self.links.keychain.clone(),
                    on,
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
                }
            });
            lp.repo = done.repo;
            lp.take_allowed_commands(&self.links.item_commands, done.settings);
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
        let paused = self.paused();
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
        for (i, items) in read_items.iter().enumerate() {
            if let Some(items) = items
                && self.live(&self.projects[i])
            {
                self.projects[i].backlog_items = items.len();
                let give_back = self.projects[i].flags.on("cursor-give-back").await;
                let machine = self.links.machine.clone();
                self.projects[i].readopt(items, &mut self.pool, give_back.then_some(machine.as_str()));
                self.projects[i].follow_drafts(items, &self.links).await;
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
            let cant_start = lp.start_blocked(&shared!(self, false), &self.pool).await;
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
        for (lp, ready) in self.projects.iter_mut().zip(ready) {
            if *ready && !lp.week_checked {
                lp.week_checked = true;
                if ProjectLoop::weekly_on(&lp.flags).await {
                    for usage in [lp.store.usage(), lp.store.week_usage()].into_iter().flatten() {
                        ProjectLoop::cut_off_for_week(&lp.id, &usage, &mut self.engine_limits);
                    }
                }
            }
        }
        let stopping = self.stopping();
        let starter = self.starter.clone();
        let live: Vec<bool> = (self.projects.iter().zip(ready))
            .map(|(lp, &ready)| ready && self.live(lp))
            .collect();
        if starter.is_some() {
            self.settle_outcomes().await;
        }
        self.pace_step(ready, &live).await;
        let concurrency = self.room_total(self.report_concurrency());
        if let Some(starter) = starter.as_deref() {
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live {
                    if !paused && !stopping {
                        lp.release_silenced(concurrency.saturating_sub(starter.active()));
                    }
                    lp.resume_sessions(&mut self.pool, starter, &mut shared!(self, stopping))
                        .await;
                    lp.take_finished(&mut self.pool, &self.links).await;
                    if stopping {
                        lp.cut_steps();
                    }
                    lp.carry_out_lane_commands(&mut self.pool, starter, &self.links, paused || stopping);
                    if !paused {
                        let room = concurrency.saturating_sub(starter.active());
                        lp.carry_out_commands(&mut self.pool, starter, room, &mut shared!(self, stopping))
                            .await;
                        lp.advance(&self.pool, starter);
                        let mut room = concurrency.saturating_sub(starter.active());
                        lp.triage_pass(&mut self.pool, starter, &mut room, &mut shared!(self, stopping))
                            .await;
                    }
                }
            }
        }
        let mut slots = concurrency.saturating_sub(starter.as_ref().map_or(0, |s| s.active()));
        // Asks have a person waiting: they take their slots first, never wait for one, and count toward `concurrency`.
        if let (Some(host), Some(_)) = (self.asks.clone(), starter.as_ref()) {
            slots = slots.saturating_sub(self.projects.iter_mut().map(|lp| lp.asking()).sum());
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live {
                    let shared = shared!(self, stopping);
                    lp.ask_pass(paused, &self.pool, &mut slots, &host, &shared).await;
                }
            }
        }
        // A person waiting on Build this: written up as a draft in the store and answered here, ahead of any new work.
        for (lp, &live) in self.projects.iter_mut().zip(&live) {
            if live {
                let shared = Shared {
                    links: &self.links,
                    local: &self.local,
                    claim_paused_until: &mut self.claim_paused_until,
                    claude_signed_out_until: &mut self.claude_signed_out_until,
                    engine_limits: &mut self.engine_limits,
                    stopping,
                };
                lp.idea_pass(&shared).await;
                lp.gallery_feedback_pass(&shared).await;
            }
        }
        // Drafts jump the queue, so they take their slots before any item is planned, and count toward `concurrency`.
        if let (Some(host), Some(_)) = (self.drafts.clone(), starter.as_ref()) {
            slots = slots.saturating_sub(
                self.projects
                    .iter_mut()
                    .filter_map(|lp| lp.drafting().then_some(()))
                    .count() as u32,
            );
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live && !paused {
                    let shared = shared!(self, stopping);
                    lp.draft_pass(&mut slots, &host, &shared, self.pool.rules().lanes).await;
                }
                if live {
                    let shared = shared!(self, stopping);
                    lp.land_drafts(&host, &shared, self.merge_wait, self.pool.rules().lanes)
                        .await;
                    let drafting = lp.drafting().then(|| lp.draft.as_ref().map(|d| d.id.clone())).flatten();
                    lp.live.set_drafting(&lp.id, drafting);
                }
            }
        }
        // The trial plan comes after asks and drafts and before items are planned; it counts toward `concurrency` too.
        if let (Some((host, checks)), Some(_)) = (self.trial.clone(), starter.as_ref()) {
            slots = slots.saturating_sub(self.projects.iter().filter(|lp| lp.trialing()).count() as u32);
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live {
                    let shared = shared!(self, stopping);
                    lp.trial_pass(&mut self.pool, &mut slots, paused, &host, &checks, &shared)
                        .await;
                }
            }
        }
        let mut candidates: Vec<Option<Candidate>> = (self.projects.iter().zip(ready).zip(paused))
            .map(|((lp, &ready), &paused)| {
                ready.then(|| Candidate {
                    id: &lp.id,
                    paused,
                    runs: lp.runs.clone(),
                    cfg: &lp.project,
                    rush: (lp.items.values())
                        .filter(|i| i.do_next)
                        .map(|i| i.id.clone())
                        .collect(),
                })
            })
            .collect();
        let (plans, last) = plan(&self.pool, self.last_served, slots, &mut candidates);
        drop(candidates);
        self.last_served = last;
        let mut scheduled = Vec::new();
        // Claude implementer starts this pass, over every project: the model slots are shared by the computer.
        let mut claude_starts = 0;
        for ((lp, actions), live) in self.projects.iter_mut().zip(plans).zip(live.iter().copied()) {
            let mut answer = None;
            lp.claude_starts = claude_starts;
            if let Some(starter) = starter.as_deref().filter(|_| live) {
                slots = slots.saturating_sub(
                    lp.apply_starts(&actions, &mut self.pool, starter, &mut shared!(self, stopping))
                        .await,
                );
                answer = Some(if actions.contains(&Action::AskForWork) {
                    lp.claim(&mut self.pool, &mut slots, starter, &mut shared!(self, stopping))
                        .await
                } else {
                    None
                });
            }
            claude_starts = lp.claude_starts;
            scheduled.push((actions, answer));
        }
        self.auto.work_waiting = slots == 0 && starter.is_some() && self.work_waits(&live, paused);
        scheduled
    }
}

/// What `spawn_all` starts the orchestrator from.
pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub lane_count: u32,
    /// `concurrency`: how many sessions may run at once, across every project.
    pub concurrency: u32,
    /// The computer's local model, shared with the session starters.
    pub local: crate::local_model::LocalModel,
    /// `autoConcurrency`.
    pub auto_concurrency: AutoConcurrency,
    /// `pollIntervalSec`: how often the loop steps between the minute's passes.
    pub poll_every: Duration,
    pub backlog_url: &'a str,
    pub keychain: &'a Arc<dyn TokenStore>,
    pub paused: &'a PauseState,
    pub item_commands: &'a ItemCommands,
    pub work: &'a crate::work_signal::WorkSignals,
    pub live: &'a LiveHandle,
    pub guard: Option<&'a PortGuard>,
    pub machine: String,
    pub environment: String,
    pub tags: Vec<String>,
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

/// The latest setup checks (`runtime::spawn_setup_checks`).
pub type SetupChecksHandle = Arc<std::sync::Mutex<Option<crate::setup_check::SetupChecks>>>;

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
    machine = machine
        .with_local(args.local)
        .with_auto(args.auto_concurrency)
        .with_machine_dir(args.dir);
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
            machine.ideas_rounds(&steps).await;
            machine.deprecation_checks(&steps).await;
            machine.vet_rounds(&steps).await;
        } else {
            machine.poll().await;
        }
        machine.run_passes().await;
    }
}
