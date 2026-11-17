//! Pool tasks (docs/specs/pools.md; the TypeScript agent's `poolReport.ts` and `poolTasks.ts`): the store sends this
//! computer its work. Once a minute the project's loop reports `health` with `obeys: true`, and the answer's tasks are
//! what starts: an item the store already claimed for this computer is claimed by name and goes through the item flow
//! (`take_lease`, then the usual starts); every chore (`driver/chores.rs`) is a local item with the task's id and no lease, through the same flow. A task is the item's whole chain, so it ends
//! (`task_end`) when the item leaves the computer's sessions (`pool_tasks::task_state`). Nothing else starts here:
//! no `claim_next`, and nothing when the store can't be reached (fail closed). An end the store has not heard is kept in
//! the state file and sent again, also after a restart (the owner's outage rule). Claim mode only.
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use serde_json::{Value, json};

use super::Shared;
use super::project::{ProjectLoop, SessionStarter};
use super::start_gate::StartGate;
use crate::backlog::pool::{Health, PoolUsage, RunningTask, TaskEnd, TaskInput};
use crate::backlog::{BacklogError, ClaimNextAgent};
use crate::engines::EngineId;
use crate::item_lifecycle::ItemState;
use crate::jobs::host::JobHost;
use crate::lane_pool::LanePool;
use crate::orchestrator::Action;
use crate::pool_tasks::{Kind, OwedTaskEnd, REFUSED, TaskRuns, TaskState, classify, is_smoke, task_state};
use crate::store::Store;
use crate::telemetry::event::{self, Event};
use std::sync::Arc;

/// How often `health` is sent (`REPORT_EVERY_MS` in poolReport.ts).
pub(super) const REPORT_EVERY: Duration = Duration::from_secs(60);
/// How long after a `task_end` the store didn't take it, it is sent again.
const OWED_RETRY: Duration = Duration::from_secs(60);

/// What the store's last `health` answer said of whether this computer may start work: its `wait` (pools-on-server.md
/// section 14.4). Unknown until the first answer and again after one that could not be had, so a store that can't be read
/// starts nothing (fail closed); the agent decides none of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) enum Wait {
    #[default]
    Unknown,
    Clear,
    /// The store's words for why nothing is sent now (paused, restarting, the budget, an own failure, the windows).
    Held(String),
}

/// A project's pool state: the tasks, the ends owed and when each was last tried.
pub(super) struct Pool {
    pub(super) runs: TaskRuns,
    /// The store's `wait` as of its last answer.
    pub(super) wait: Wait,
    owed: Vec<OwedTaskEnd>,
    reported: Option<Instant>,
    /// How often `health` goes out ([`REPORT_EVERY`]; a test sets it shorter).
    pub(super) every: Duration,
    owed_tried: Option<Instant>,
    /// What the state file holds, so a change is saved once.
    saved: Option<(
        std::collections::BTreeMap<String, crate::pool_tasks::PoolTask>,
        Vec<OwedTaskEnd>,
    )>,
}

impl Default for Pool {
    fn default() -> Self {
        Self {
            runs: TaskRuns::default(),
            wait: Wait::Unknown,
            owed: Vec::new(),
            reported: None,
            every: REPORT_EVERY,
            owed_tried: None,
            saved: None,
        }
    }
}

impl Pool {
    /// Lets the next pass send the ends owed again at once, as if the minute had passed (tests).
    #[cfg(test)]
    pub(super) fn retry_owed_now(&mut self) {
        self.owed_tried = None;
    }

    /// The tasks running and the ends owed as the state file kept them.
    pub(super) fn restoring(store: &Store) -> Self {
        let (tasks, owed) = (store.pool_tasks(), store.task_ends_owed());
        Self {
            runs: TaskRuns::restoring(tasks.clone()),
            saved: Some((tasks, owed.clone())),
            owed,
            ..Self::default()
        }
    }
}

impl ProjectLoop {
    /// The pool's part of a pass: sends the ends owed, ends the tasks whose items have left the sessions, and, once a
    /// minute (at once on a `work_available`), reports `health` and starts the tasks it answers. Always reports, paused or
    /// not: the report renews the lease of every task that runs. `None` when nothing was asked, else the answer: the
    /// item claimed last, `none`, or `failed` (the tick's `claim`).
    pub(super) async fn pool_pass(
        &mut self,
        paused: bool,
        lanes: &mut LanePool,
        slots: &mut u32,
        starter: &dyn SessionStarter,
        jobs: Option<&Arc<dyn JobHost>>,
        shared: &mut Shared<'_>,
    ) -> Option<String> {
        let repo = self.repo?;
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return None,
            Err(e) => {
                self.cant_start(format!("reading the backlog link: {e:#}"));
                return None;
            }
        };
        // The first pass adopts the leases a restart left; a task must not look finished before its item is back.
        if self.adopted {
            self.end_finished_tasks();
        }
        // The jobs the store sent: the one that finished ends, the next one starts.
        self.jobs_pass(jobs, shared.links, paused, shared.stopping, slots).await;
        self.smokes_pass(shared.stopping).await;
        self.send_owed_ends(repo, &token).await;
        // This computer takes the store's tasks, so it may ask for the ones only it can see (`chore_trigger`).
        self.takes_tasks = true;
        if self.adopted && !paused && !shared.stopping {
            self.look_at_main(repo, &token, lanes, shared).await;
        }
        let woken = shared.links.work.signal(&self.id).notified().now_or_never().is_some();
        if !woken && self.pool.reported.is_some_and(|at| at.elapsed() < self.pool.every) {
            return None;
        }
        self.pool.reported = Some(Instant::now());
        // The old claim's rule for taking work: a free lane that may start, a session slot, and the model's cap not full;
        // and nothing while this computer's pause or the store's is on, or the store's is unknown (it fails closed).
        let gate = StartGate::new(self, lanes, shared);
        let model_full = self.model_full(starter) || self.local_queue_full(shared);
        let asks = (self.decide_gated(lanes, shared, &gate, model_full)).contains(&Action::AskForWork);
        let mut refused = Self::own_refusal(shared, asks);
        // Another agent on this computer's port holds the work (`guard.rs`).
        let guard = self.guard.clone();
        if let (None, Some(guard)) = (&refused, guard)
            && let Some(why) = guard.refusal().await
        {
            tracing::error!("{}: taking no tasks: {why}", self.id);
            refused = Some(why.chars().take(200).collect());
        }
        let report = self.health(paused, refused.clone(), jobs.is_some(), shared);
        let answer = match self.client.health(repo, Some(&token), &report).await {
            Ok(answer) => answer,
            Err(e) => {
                // Unreachable, or an older store: nothing is started, and scheduling never falls back.
                self.store_error("health", &e);
                self.pool.wait = Wait::Unknown;
                return Some(self.health_answered("failed"));
            }
        };
        self.pool.wait = answer.wait.clone().map_or(Wait::Clear, Wait::Held);
        // The store's numbers: the project's, and the computer-wide ones about limits. An answer with none leaves them unknown.
        self.take_store_tuning(answer.tuning);
        shared.engine_limits.take_numbers(answer.tuning.as_ref());
        // The store decided what becomes of the chores this computer parked: close them, free their lanes, end their tasks.
        self.stop_parked_chores(&answer.stop, lanes, starter, shared).await;
        // A smoke takes no slot and no lane, so it starts whatever else holds this computer back (not a stopping one).
        if !shared.stopping {
            self.start_smoke_tasks(&answer.tasks);
        }
        // Held off for this computer's own reasons (a restart, a usage limit an engine hit, every session slot or lane taken,
        // the model's cap) or the store's (`wait`: a pause, the budget, an own failure, the windows): the tasks come again with
        // a later report. The store's reasons are read here, not worked out again.
        if refused.is_some()
            || paused
            || shared.stopping
            || !self.adopted
            || *slots == 0
            || !asks
            || self.pool.wait != Wait::Clear
            || self.project.tuning.store.is_none()
        {
            return Some(self.health_answered("none"));
        }
        let fresh: Vec<TaskInput> = self.pool.runs.fresh(&answer.tasks).into_iter().cloned().collect();
        let mut last = None;
        for task in fresh {
            match classify(&task) {
                Kind::Item(item) => {
                    if let Some(id) = self.take_item_task(repo, &token, &task, &item, shared).await {
                        last = Some(id);
                    }
                }
                Kind::Unknown(run) => {
                    let note = format!("This agent doesn't know what a {run} task is.");
                    tracing::warn!("{}: task {} ({}): {note}", self.id, task.id, task.subject);
                    self.end_task(TaskEnd {
                        note: Some(note),
                        ..TaskEnd::done(&task.id, false)
                    });
                }
                Kind::Job(_) => {
                    if let Some(job) = &task.job {
                        self.job_task(&task, job, jobs);
                    }
                }
                Kind::Chore(kind) => {
                    if self.chore_task(&task, &kind, shared).await {
                        last = Some(task.id.clone());
                    }
                }
            }
        }
        if last.is_some() {
            let gate = StartGate::new(self, lanes, shared);
            let model_full = self.model_full(starter);
            let actions = self.decide_gated(lanes, shared, &gate, model_full);
            *slots = slots.saturating_sub(self.apply_starts(&actions, lanes, starter, shared).await);
        }
        self.send_owed_ends(repo, &token).await;
        self.save_pool();
        Some(self.health_answered(&last.unwrap_or_else(|| "none".to_string())))
    }

    /// Starts the smoke tasks of an answer not seen before (`smoke_task.rs`). Only these are marked seen here: the rest wait
    /// for the gate below.
    fn start_smoke_tasks(&mut self, tasks: &[TaskInput]) {
        let smokes: Vec<TaskInput> = tasks.iter().filter(|t| is_smoke(t)).cloned().collect();
        let fresh: Vec<TaskInput> = self.pool.runs.fresh(&smokes).into_iter().cloned().collect();
        for task in fresh {
            if let Some(job) = &task.job {
                self.smoke_task(&task, job);
            }
        }
    }

    /// The base branch's looks that end in a `chore_trigger`: the baseline and main's CI when nothing is queued here (a red
    /// main is reported, `fix_main.rs`), how far main moved ahead (`main_sync.rs`) and the CI coverage fact.
    async fn look_at_main(&mut self, repo: u64, token: &str, lanes: &LanePool, shared: &Shared<'_>) {
        let (now, machine) = (Instant::now(), shared.links.machine.clone());
        if !self.runs.values().any(|r| r.state == ItemState::Queued) {
            self.idle_baseline(repo, Some(token), lanes, &machine, now).await;
        }
        self.main_sync(repo, Some(token), &machine, false, now).await;
        self.look_at_ci_coverage(now).await;
    }

    /// The `claim` event for a `health` answer: the item claimed last, `none` or `failed`.
    fn health_answered(&mut self, answer: &str) -> String {
        self.events.add_in(
            &self.id,
            Event::Claim(event::Claim {
                op: Some("health".into()),
                answer: Some(json!(answer)),
                ..Default::default()
            }),
        );
        answer.to_string()
    }

    /// The item a task names, claimed by name (the store answers its own lease token) and queued to start. `None` when it
    /// was not claimed: it was here already, the store refused (the task ends), or it could not be asked (it comes again).
    async fn take_item_task(
        &mut self,
        repo: u64,
        token: &str,
        task: &TaskInput,
        item: &str,
        shared: &Shared<'_>,
    ) -> Option<String> {
        if self.runs.contains_key(item) {
            self.pool.runs.start(&task.id, item, task.tier);
            return None;
        }
        let agent = ClaimNextAgent {
            machine: shared.links.machine.clone(),
            environment: shared.links.environment.clone(),
            base_branch: self.project.base_branch.clone(),
            tags: shared.links.tags.get(),
            week: self.week_use(),
        };
        match self.client.claim_task(repo, Some(token), item, &agent).await {
            Ok(claimed) => {
                let mut handed = claimed.item;
                handed.tier = Some(task.tier.item_tier().to_string());
                self.take_lease(&handed, claimed.token, ItemState::Queued);
                self.pool.runs.start(&task.id, item, task.tier);
                Some(item.to_string())
            }
            Err(e) if refused_for_good(&e) => {
                tracing::info!("{}: task {} ({item}) couldn't start: {e}", self.id, task.id);
                self.end_task(TaskEnd::done(&task.id, false));
                None
            }
            Err(e) => {
                self.store_error("claim", &e);
                self.pool.runs.forget(&task.id);
                None
            }
        }
    }

    /// Why this computer takes no new work for its own reasons now (`refused` in `health`): Claude signed out with no lane
    /// left on another engine (`can_start`: some lane may start a new session). An own failure is no refusal: it is reported
    /// as a fact (`ownFailure`) and the store holds the pause.
    fn own_refusal(shared: &Shared<'_>, can_start: bool) -> Option<String> {
        let now = Instant::now();
        (!can_start && shared.claude_signed_out_until.is_some_and(|until| now < until)).then(|| "signed out".into())
    }

    /// The body of `health` (`PoolReporter.report`): the tasks that run, usage, limits and spend, all facts.
    fn health(&self, paused: bool, refused: Option<String>, jobs: bool, shared: &Shared<'_>) -> Health {
        let now = Instant::now();
        let now_ms = crate::telemetry::now_ms();
        let limits = [EngineId::Claude, EngineId::Cursor, EngineId::Pi, EngineId::Codex]
            .into_iter()
            .filter_map(|engine| {
                let left = shared
                    .engine_limits
                    .limited_until(engine, now)?
                    .saturating_duration_since(now);
                Some((
                    engine.to_string(),
                    now_ms + i64::try_from(left.as_millis()).unwrap_or(0),
                ))
            })
            .collect();
        let spent = self.data_dir.as_deref().map_or(0.0, |dir| {
            crate::store::MachineStore::load(dir).map_or(0.0, |m| m.spend_today(&self.id))
        });
        Health {
            obeys: true,
            // What this computer can run for a job, as facts (`jobCan`): nothing when no job runner is wired.
            can: if jobs {
                crate::jobs::inputs::can_for(&self.project, &self.jobs.chrome, &shared.links.env)
                    .iter()
                    .map(|c| c.name().to_string())
                    .collect()
            } else {
                Vec::new()
            },
            project: self.id.clone(),
            agent: ClaimNextAgent {
                machine: shared.links.machine.clone(),
                environment: shared.links.environment.clone(),
                base_branch: self.project.base_branch.clone(),
                tags: shared.links.tags.get(),
                week: self.week_use(),
            },
            // A chore that stopped is reported `needs_human` with why, and waits for the store's `stop`.
            running: (self.pool.runs.running_ids())
                .into_iter()
                .map(
                    |id| match (self.pool.runs.running().get(&id)).and_then(|t| self.parked_chore(&t.item)) {
                        Some(why) => RunningTask::stopped(id, why),
                        None => RunningTask::running(id),
                    },
                )
                .collect(),
            usage: self
                .store
                .usage()
                .as_ref()
                .and_then(|u| pool_usage(u, crate::telemetry::now_secs())),
            limits,
            spend: std::collections::BTreeMap::from([(self.id.clone(), spent)]),
            refused,
            paused,
            draining: shared.stopping,
            own_failure: shared.own_failure.clone(),
        }
    }

    /// Ends the tasks whose items have left the computer's sessions, to be sent with the next pass.
    pub(super) fn end_finished_tasks(&mut self) {
        let parked: std::collections::HashSet<String> = (self.pool.runs.running().values())
            .map(|t| t.item.clone())
            .filter(|item| self.parked_chore(item).is_some())
            .collect();
        let runs = &self.runs;
        let ended = self.pool.runs.ended(|item| {
            let run = runs.get(item)?;
            Some(if parked.contains(item) {
                TaskState::Parked
            } else {
                task_state(run.state)
            })
        });
        for end in ended {
            self.end_task(end);
        }
        self.forget_finished_chores();
        self.save_pool();
    }

    /// Keeps `end` to send: on disk before it is sent, and sent again until the store takes it.
    pub(super) fn end_task(&mut self, end: TaskEnd) {
        let since = self.now_secs();
        self.pool.owed.retain(|o| o.end.id != end.id);
        self.pool.owed.push(OwedTaskEnd { end, since });
        self.pool.owed_tried = None;
    }

    /// Sends the ends the store hasn't taken. One it answers `held: false` (its lease ran out first) or refuses is
    /// dropped; one it can't be reached for is kept, and the rest wait for it.
    async fn send_owed_ends(&mut self, repo: u64, token: &str) {
        let now = self.now_secs();
        self.pool.owed.retain(|o| !o.stale(now));
        if self.pool.owed.is_empty() || self.pool.owed_tried.is_some_and(|at| at.elapsed() < OWED_RETRY) {
            return;
        }
        self.pool.owed_tried = Some(Instant::now());
        self.save_pool();
        for owed in self.pool.owed.clone() {
            match self.client.task_end(repo, Some(token), &owed.end).await {
                Ok(answer) => {
                    if !answer.held {
                        tracing::info!("{}: the store had let go of task {} already", self.id, owed.end.id);
                    }
                }
                Err(e) if e.is_refusal() => {
                    tracing::warn!("{}: the store refused the end of task {}: {e}", self.id, owed.end.id);
                }
                Err(e) => {
                    self.store_error("task_end", &e);
                    break;
                }
            }
            self.pool.owed.retain(|o| o.end.id != owed.end.id);
        }
        self.save_pool();
    }

    /// Writes the tasks and the ends owed to the state file when they differ from what it holds.
    pub(super) fn save_pool(&mut self) {
        let now = (self.pool.runs.running().clone(), self.pool.owed.clone());
        if self.pool.saved.as_ref() == Some(&now) {
            return;
        }
        self.store.set_pool_tasks(&now.0);
        self.store.set_task_ends_owed(&now.1);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the tasks failed: {e:#}", self.id);
        }
        self.pool.saved = Some(now);
    }

    /// The tier the store picked for `item`'s task, given back to a lease taken again after a restart.
    pub(super) fn restore_task_tiers(&mut self) {
        for task in self.pool.runs.running().values() {
            if let Some(item) = self.items.get_mut(&task.item)
                && item.tier.is_none()
            {
                item.tier = Some(task.tier.clone());
            }
        }
    }
}

/// The store refused for good: sending the same again can't change that.
fn refused_for_good(e: &BacklogError) -> bool {
    e.code().is_some_and(|code| REFUSED.contains(&code))
}

/// Claude's five-hour reading as `health` takes it: shares from 0 to 1 and resets in epoch seconds, and only while its
/// window is still open ("a reading that is no longer current is unknown"). `None` when there is none to send.
fn pool_usage(usage: &Value, now_secs: i64) -> Option<PoolUsage> {
    let five = usage
        .get("unifiedWindows")
        .and_then(|w| w.get("five_hour"))
        .unwrap_or(usage);
    let resets_at = five.get("resetsAt").and_then(Value::as_i64)?;
    if resets_at <= now_secs {
        return None;
    }
    let share = |v: Option<f64>| v.map(crate::tiers::frac).filter(|n| (0.0..=1.0).contains(n));
    let (week, week_resets) = crate::tiers::week_reading(usage).unwrap_or((None, None));
    Some(PoolUsage {
        status: usage
            .get("status")
            .or_else(|| five.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .take(40)
            .collect(),
        utilization: share(five.get("utilization").and_then(Value::as_f64)),
        resets_at: Some(resets_at),
        week_utilization: share(week),
        week_resets_at: week_resets.filter(|&t| t > now_secs),
    })
}
