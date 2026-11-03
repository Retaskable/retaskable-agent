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

use super::{
    OrchestratorMode, OutcomeSender, ProjectLoop, RunRequest, RunStarter, SessionStarter, Step, playtest_round,
};
use crate::backlog::runs::RunKind;
use crate::backlog::{BacklogError, Client, Item};
use crate::config::{EngineChoiceConfig, Playtests, ProjectConfig};
use crate::env::Env;
use crate::lane_pool::{LanePool, PoolRules};
use crate::lanes;
use crate::link::TokenStore;
use crate::orchestrator::Action;
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
async fn repo_id(github_repo: &str) -> Option<u64> {
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

/// Why a project's items weren't read this pass.
enum Fail {
    /// Before the store could be asked (no repo id, no link): the page hears of it, no `store_error` event.
    Cant(String),
    Store(BacklogError),
}

/// A project's backlog read: the repo id it learned and the items or why not.
struct Read {
    repo: Option<u64>,
    items: Result<Vec<Item>, Fail>,
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
}

/// `links` and the computer-wide gates ([`Machine::claim_paused_until`], [`Machine::claude_signed_out_until`]),
/// borrowed together: what a project's claim and start methods need from the [`Machine`] besides the lane
/// pool, bundled so threading them through doesn't grow those methods past clippy's argument limit.
pub struct Shared<'a> {
    pub links: &'a Links,
    pub claim_paused_until: &'a mut Option<Instant>,
    pub claude_signed_out_until: &'a mut Option<Instant>,
    /// [`process_tree::stopping`] as of this pass's start: a restart or Quit cutting every
    /// session, so no project claims or starts new work either
    /// (docs/specs/agent/07.4-stopping-and-restarting.md, `restart-cuts-sessions`).
    pub stopping: bool,
}

/// Reads one project's items. Owns what it needs, so it runs as its own task: a panic in it is that
/// project's failed read and nothing else's.
async fn read(
    client: Arc<Client>,
    id: String,
    github_repo: String,
    cached: Option<u64>,
    backlog_url: String,
    keychain: Arc<dyn TokenStore>,
) -> Read {
    let repo = match cached {
        Some(repo) => Some(repo),
        None => repo_id(&github_repo).await,
    };
    let Some(repo_id) = repo else {
        tracing::warn!("{id}: dry tick skipped: gh couldn't say the id of {github_repo}");
        let items = Err(Fail::Cant(format!("gh couldn't say the id of {github_repo}")));
        return Read { repo, items };
    };
    let token = match keychain.get(&backlog_url) {
        Ok(token) => token,
        Err(e) => {
            tracing::warn!("{id}: dry tick skipped: reading the backlog link: {e:#}");
            let items = Err(Fail::Cant(format!("reading the backlog link: {e:#}")));
            return Read { repo, items };
        }
    };
    let items = match token {
        None => Err(Fail::Store(BacklogError::BadAnswer {
            status: 401,
            code: Some("unauthorized".into()),
            message: "this computer holds no link to the backlog store".into(),
        })),
        Some(token) => client.list(repo_id, Some(&token)).await.map_err(Fail::Store),
    };
    Read { repo, items }
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
    pub(super) projects: Vec<ProjectLoop>,
    /// Index in `projects` of the project served last: the next start goes to the one after it.
    pub(super) last_served: Option<usize>,
    pub(super) links: Links,
    /// Runs the sessions for what is claimed. With none, `claim` mode runs dry.
    pub(super) starter: Option<Arc<dyn SessionStarter>>,
    /// Runs spec drafts. With none, no draft is claimed.
    pub(super) drafts: Option<Arc<dyn super::DraftHost>>,
    /// The wait between merge tries after a draft is rebased.
    pub(super) merge_wait: std::time::Duration,
    /// Does the work of runs in `runs` mode. With none, that mode drops old runs and asks for nothing.
    pub(super) run_starters: RunStarters,
    /// How many sessions may run at once, across every project (`concurrency`).
    pub(super) concurrency: u32,
    /// Sessions report their end here; the receiver is read on each wake.
    pub(super) outcomes: mpsc::UnboundedReceiver<super::SessionOutcome>,
    pub(super) outcome_sender: OutcomeSender,
    /// Stands in for [`process_tree::stopping`] in a test, which can't set that process-wide
    /// flag itself without also exposing every other test in the binary to it. `None` outside
    /// tests, always: production always reads the real flag.
    #[cfg(test)]
    pub(super) stopping_override: Option<bool>,
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
            projects: Vec::new(),
            last_served: None,
            links,
            starter: None,
            drafts: None,
            merge_wait: super::land::MERGE_WAIT,
            run_starters: RunStarters::default(),
            concurrency,
            outcomes,
            outcome_sender: OutcomeSender(sender),
            #[cfg(test)]
            stopping_override: None,
        }
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
                self.live(lp)
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
                Some((
                    lp.playtest_round_running.clone(),
                    playtest_round::Job {
                        id: lp.id.clone(),
                        client: lp.client.clone(),
                        repo: lp.repo,
                        project: lp.project.clone(),
                        env: self.links.env.clone(),
                        engine_choice: self.links.engine_choice.clone(),
                        machine: self.links.machine.clone(),
                        backlog_url: self.links.backlog_url.clone(),
                        keychain: self.links.keychain.clone(),
                    },
                ))
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
        for (lp, paused) in self.projects.iter_mut().zip(paused) {
            if lp.mode != OrchestratorMode::Runs {
                continue;
            }
            let shared = Shared {
                links: &self.links,
                claim_paused_until: &mut claim_paused_until,
                claude_signed_out_until: &mut signed_out_until,
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

    /// Lets `claim` mode claim and run spec drafts.
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
        let jobs: Vec<_> = self
            .projects
            .iter()
            .map(|lp| {
                tokio::spawn(read(
                    lp.client.clone(),
                    lp.id.clone(),
                    lp.project.github_repo.clone(),
                    lp.repo,
                    self.links.backlog_url.clone(),
                    self.links.keychain.clone(),
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
                }
            });
            lp.repo = done.repo;
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
                self.projects[i].readopt(items, &mut self.pool);
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
            match items {
                Some(items) => steps.push(lp.record(items, paused, actions, &self.pool, live)),
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
        let stopping = self.stopping();
        let starter = self.starter.clone();
        let live: Vec<bool> = (self.projects.iter().zip(ready))
            .map(|(lp, &ready)| ready && self.live(lp))
            .collect();
        if let Some(starter) = starter.as_deref() {
            self.settle_outcomes(starter).await;
            for ((lp, &live), &paused) in self.projects.iter_mut().zip(&live).zip(paused) {
                if live {
                    lp.resume_sessions(
                        &mut self.pool,
                        starter,
                        &mut Shared {
                            links: &self.links,
                            claim_paused_until: &mut self.claim_paused_until,
                            claude_signed_out_until: &mut self.claude_signed_out_until,
                            stopping,
                        },
                    )
                    .await;
                    lp.take_finished(&mut self.pool, &self.links).await;
                    lp.carry_out_lane_commands(&mut self.pool, starter, &self.links, paused || stopping);
                    if !paused {
                        let room = self.concurrency.saturating_sub(starter.active());
                        lp.carry_out_commands(
                            &mut self.pool,
                            starter,
                            room,
                            &mut Shared {
                                links: &self.links,
                                claim_paused_until: &mut self.claim_paused_until,
                                claude_signed_out_until: &mut self.claude_signed_out_until,
                                stopping,
                            },
                        )
                        .await;
                        lp.advance(&self.pool, starter);
                    }
                }
            }
        }
        let mut slots = (self.concurrency).saturating_sub(starter.as_ref().map_or(0, |s| s.active()));
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
                    let shared = Shared {
                        links: &self.links,
                        claim_paused_until: &mut self.claim_paused_until,
                        claude_signed_out_until: &mut self.claude_signed_out_until,
                        stopping,
                    };
                    lp.draft_pass(&mut slots, &host, &shared).await;
                }
                if live {
                    let shared = Shared {
                        links: &self.links,
                        claim_paused_until: &mut self.claim_paused_until,
                        claude_signed_out_until: &mut self.claude_signed_out_until,
                        stopping,
                    };
                    lp.land_drafts(&host, &shared, self.merge_wait).await;
                    let drafting = lp.drafting().then(|| lp.draft.as_ref().map(|d| d.id.clone())).flatten();
                    lp.live.set_drafting(&lp.id, drafting);
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
        for ((lp, actions), live) in self.projects.iter_mut().zip(plans).zip(live) {
            let mut answer = None;
            if let Some(starter) = starter.as_deref().filter(|_| live) {
                slots = slots.saturating_sub(
                    lp.apply_starts(
                        &actions,
                        &mut self.pool,
                        starter,
                        &mut Shared {
                            links: &self.links,
                            claim_paused_until: &mut self.claim_paused_until,
                            claude_signed_out_until: &mut self.claude_signed_out_until,
                            stopping,
                        },
                    )
                    .await,
                );
                answer = Some(if actions.contains(&Action::AskForWork) {
                    lp.claim(
                        &mut self.pool,
                        &mut slots,
                        starter,
                        &mut Shared {
                            links: &self.links,
                            claim_paused_until: &mut self.claim_paused_until,
                            claude_signed_out_until: &mut self.claude_signed_out_until,
                            stopping,
                        },
                    )
                    .await
                } else {
                    None
                });
            }
            scheduled.push((actions, answer));
        }
        scheduled
    }
}

/// The starters `runs` mode has, as one: asks for the kinds any of them can run and hands a run to the one that can.
#[derive(Default)]
pub(super) struct RunStarters(Vec<Arc<dyn RunStarter>>);

impl RunStarter for RunStarters {
    fn kinds(&self) -> Vec<RunKind> {
        let mut kinds: Vec<RunKind> = Vec::new();
        for kind in self.0.iter().flat_map(|s| s.kinds()) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        match self.0.iter().find(|s| s.kinds().contains(&request.kind)) {
            Some(starter) => starter.start(request),
            None => Err(format!("no runner for {:?} runs", request.kind)),
        }
    }
}

/// What a caught panic said.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    (panic.downcast_ref::<&str>().map(ToString::to_string))
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// What `spawn_all` starts the orchestrator from.
pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub lane_count: u32,
    /// `concurrency`: how many sessions may run at once, across every project.
    pub concurrency: u32,
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
    /// The runners of `runs` mode, one per set of kinds. Empty until the kind runners are built.
    pub run_starters: Vec<Arc<dyn RunStarter>>,
    /// `MachineConfig::max_resume_context_tokens`: a resumed session over this size starts fresh instead.
    pub max_resume_context_tokens: u32,
    /// Read for `RETASKABLE_CLAUDE`/`PATH` when a playtest grouping round resolves its Claude binary.
    pub env: Env,
    /// Which engine a playtest grouping round's `Role::Planner` session runs on.
    pub engine_choice: EngineChoiceConfig,
}

/// Starts the computer's one orchestrator task when the mode isn't `Off`. A project that can't loop (no
/// GitHub repo, unreadable state) is logged and left out; the others go on.
pub fn spawn_all(args: SpawnArgs) {
    if args.live.snapshot().mode == OrchestratorMode::Off {
        return;
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
        },
    );
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
        } else {
            machine.poll().await;
        }
        machine.run_passes().await;
    }
}
