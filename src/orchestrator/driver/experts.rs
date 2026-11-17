//! A project's expert sessions (docs/specs/expert-chat.md; Retaskable.com's `agent/src/expert/`): a person's chat with
//! Claude in a lane of its own, started by `expert_start` and driven by `expert_say`, `expert_stop` and `expert_end`
//! (`commands::expert` queues them). Each message runs one resumed turn through an [`ExpertHost`]; a message sent
//! while a turn runs waits in the store's queue (`expert_queue`, FIFO) and is run as the next turn. A stopped turn
//! keeps the queue. A session left idle for [`expert::IDLE`] parks: its branch is pushed and its lane freed, and the
//! next message takes a lane again, on the pushed branch. Every minute the loop beats the session's lease and the
//! queue's claims and reads the store's queue back (at once on the store socket's `facts_changed` naming `expert_queue`), so a message the site
//! wrote, edited or cancelled is seen. A turn counts toward `concurrency` while it runs, and is not held back by a
//! pause. The loop never plans an expert item (`read.rs` leaves them out).
//!
//! A message's pictures (the store's ids, in its queue row) are fetched when its turn starts, listed after the message
//! for the session to look at, and removed when the turn ends: no path of this computer is ever sent to the store.
//! A session can carry on a Claude Code session of this computer (`carry.rs`): its branch starts at the tip of that
//! session's, its first turn opens with a handover of the conversation, and the session is noted in the store.
//!
//! `expert_end` with `backlog` parks the session (its work pushed), queues a plan that revises the item with the
//! conversation in its request, and keeps the session as handed (`expert_handed`, `backlog.rs`): it takes no message
//! until it is parked, and when the plan merges the loop gives the lease back before the store's `revise` drops
//! `expert` from the item (`driver/revise.rs`).
//!
//! `expert_end` with `ship` (`ship.rs`) opens the session's change and hands the item to this project's own item
//! loop: it keeps the lease and the lane and goes on through checks, review and merge as any item does, in `claim`
//! mode. In `runs` mode (`ship_runs.rs`) the store takes the open change as the item's piece (`expert_ship`), so runs
//! on any computer carry it, and the session and its lane are let go.
//!
//! The session's work (what its branch changed, its commits, its pull requests) is read after each turn and carried in
//! the snapshot as `expert.work` (`work.rs`).
//!
//! Not built here: engines other than Claude.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::Shared;
use super::drafts::claim_agent;
use super::project::ProjectLoop;
use crate::ask::pictures::{self, Pictures};
use crate::backlog::expert::{ExpertItem, ShipOwed};
use crate::backlog::{ClaimNextAgent, Client};
use crate::config::ProjectConfig;
use crate::expert::claude_sessions::Continued;
use crate::expert::history;
use crate::expert::{self, Detail, Question, Reply};
use crate::expert_queue::{BEAT_MS, ExpertQueue, OwnedQueue, Queued, is_local};
use crate::implementer::publish::Published;
use crate::lane_pool::{Holder, LanePool, Want};
use crate::orchestrator::live::HeldItem;
use crate::orchestrator::live::sessions::Sessions;
use crate::view::{ExpertAction, ExpertCommand};

mod backlog;
mod carry;
mod helpers;
mod limit;
mod ship;
mod ship_runs;
mod steer;
mod work;

pub use helpers::HelperJob;
pub use work::WorkRead;

/// One turn of a session: the message to run and where.
pub struct ExpertJob {
    pub project: ProjectConfig,
    pub item: String,
    pub branch: String,
    pub lane: u32,
    /// The engine session to resume; `None` is a first turn, which starts a session with the opening prompt.
    pub session_id: Option<String>,
    /// The person's message, the first turn's prompt not included, with its pictures listed after it.
    pub message: String,
    /// The message's pictures on disk: kept until the turn is over, then removed.
    pub pictures: Pictures,
    /// Whether the lane is set up first (a new session, or one back from a park).
    pub prepare: bool,
    /// With `prepare`: start from the branch the park pushed rather than from the base.
    pub from_branch: bool,
    /// Fired to stop the turn; the host cuts the engine's processes and answers [`TurnEnd::Stopped`].
    pub stop: Arc<Notify>,
    /// The sessions running now: the turn is listed while it runs.
    pub sessions: Sessions,
    /// The open input the turn takes the person's messages on as it works (Claude only), when the host can.
    pub steer: Option<crate::engines::steer::Steer>,
}

/// A session's change to open (`ship.rs`): its work, saved and pushed as the item's own branch, and its change.
pub struct ShipJob {
    pub project: ProjectConfig,
    pub lane: u32,
    /// The session's branch, which its lane is on (or, after a park, was pushed as).
    pub branch: String,
    /// The branch the change opens from: the one the loop keeps for any item (`retaskable/<id>-<slug>-s<slice>`).
    pub target: String,
    /// Whether the lane is set up first, from the pushed branch (a parked session).
    pub prepare: bool,
    pub title: String,
    pub body: String,
}

/// A session's change as the host opened it: the change, and the commit it is at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub change: Published,
    pub head: Option<String>,
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEnd {
    /// It answered; `session_id` is what the next turn resumes.
    Done {
        session_id: String,
        reply: Reply,
    },
    Stopped,
    Failed(String),
    /// It ended at the usage limit: the queue waits for the limit to lift (`limit.rs`).
    Limited(String),
}

/// The parts of an expert session that touch the machine and the engine. Faked in tests.
pub trait ExpertHost: Send + Sync {
    /// Sets up the lane when asked and runs one turn in it.
    fn turn(&self, job: ExpertJob) -> BoxFuture<'static, TurnEnd>;
    /// Pushes the session's branch from its lane, so the lane can be freed. An `Err` keeps the lane.
    fn park(&self, project: &ProjectConfig, lane: u32, branch: &str) -> BoxFuture<'static, Result<(), String>>;
    /// The Claude Code session `id` of this computer, for a session that continues it; why not when it can't be.
    fn continuing(&self, id: &str) -> Result<Continued, String>;
    /// Starts branch `name` at the tip of the continued session's branch, pushed, when its folder is a checkout of this
    /// project. An `Err` says why it isn't, and the session starts from the base.
    fn carry_on(&self, project: &ProjectConfig, from: &Continued, name: &str)
    -> BoxFuture<'static, Result<(), String>>;
    /// The first message of a session continuing Claude Code session `id`: the latest of its conversation, where it
    /// worked, then `request`. `None` when its file can't be read any more: the person's message alone then.
    fn handover(&self, id: &str, request: &str) -> BoxFuture<'static, Option<String>>;
    /// Saves what the lane left uncommitted, pushes its work as `job.target` and opens the change, or finds the one
    /// already open for it. An `Err` says why, and leaves the session as it was.
    fn ship(&self, job: ShipJob) -> BoxFuture<'static, Result<Opened, String>>;
    /// The turns of the session's own run files (`session_id`'s `.jsonl`) and, for a session that continues one, of the
    /// Claude Code conversation it carries on: `(earlier, own)`, each oldest first (`session_history`).
    fn history(
        &self,
        project: &ProjectConfig,
        item: &str,
        session_id: Option<&str>,
        from: Option<&str>,
    ) -> BoxFuture<'static, (Vec<history::Turn>, Vec<history::Turn>)>;
    /// Reads the session's work: its lane's git (when it has a lane) and its pull requests, for `branch` and the
    /// `helpers` lane helpers' branches. A part the machine can't read is left out.
    fn work(
        &self,
        project: &ProjectConfig,
        lane: Option<u32>,
        branch: &str,
        helpers: u32,
    ) -> BoxFuture<'static, WorkRead>;
    /// Runs one helper (a scout on the lighter tier): read-only in `job.lane`, the session's own, or, with `job.own`,
    /// in a lane of its own made ready on `<from>-h<n>` from the session's last commit.
    fn helper(&self, job: HelperJob) -> BoxFuture<'static, crate::expert::helpers::Ended>;
    /// Cuts what runs in the lane and deletes the pushed branch, for a discard. Best effort.
    fn discard(&self, project: &ProjectConfig, lane: Option<u32>, branch: &str) -> BoxFuture<'static, ()>;
}

struct Turn {
    task: JoinHandle<TurnEnd>,
    stop: Arc<Notify>,
    /// The queue rows this turn is running.
    ids: Vec<String>,
    /// Its open input, while it takes messages as it works (`steer.rs`).
    steering: Option<steer::Steering>,
}

struct Session {
    started_by: String,
    title: String,
    branch: String,
    lease: String,
    lane: Option<u32>,
    session_id: Option<String>,
    prepared: bool,
    from_branch: bool,
    queue: Vec<Queued>,
    /// Rows this session has read: never queued again, whatever a late read of the store says.
    consumed: BTreeSet<String>,
    turn: Option<Turn>,
    detail: Detail,
    ask: Option<Question>,
    idle_since: Instant,
    /// Set by `expert_stop` until the person's next message: the queue waits.
    stopped: bool,
    /// The last turn was cut, so the next message says so.
    mid_step: bool,
    /// The Claude Code session this one continues, until a turn has answered: the turn that starts a conversation
    /// opens with its handover.
    handover_from: Option<String>,
    /// The plan that revises the item, while the session is handed to the backlog.
    plan: Option<String>,
    /// The item as the store added it, and the request the session began with: what a shipped change is made from.
    item: crate::backlog::Item,
    request: String,
    /// The conversation so far, oldest first: the person's messages and the answers (a handed-off plan's request).
    talk: Vec<String>,
    /// What its branch has now, for the right panel; the read in flight and when the pull requests were last read.
    work: Option<crate::expert::work::Work>,
    reading: Option<work::Reading>,
    prs_at: Option<Instant>,
    /// How many helpers it has started: its lane helpers' branches are `<branch>-h<n>`.
    helpers: u32,
    /// The helpers running or waiting for a lane, the results waiting for the session's next turn, and whether the
    /// session was idle when the newest came in (so it is told at once).
    helping: Vec<helpers::Helper>,
    results: Vec<String>,
    tell_now: bool,
    /// While set and in the future, the queue waits for Claude's usage limit to lift (`limit.rs`).
    limit_until: Option<Instant>,
}

/// A project's expert sessions, by item id.
pub(super) struct Experts {
    queue: Option<Arc<ExpertQueue<OwnedQueue>>>,
    sessions: BTreeMap<String, Session>,
    /// Sessions whose change was opened this pass: the item loop takes them over (`ship.rs`).
    shipped: Vec<ship::Shipped>,
    /// `runs` mode: the changes opened that the store hasn't taken as pieces yet, by item (saved in the state file
    /// before the op is sent, sent again every pass and after a restart: `ship_runs.rs`), and the one opened by the
    /// command running now.
    pub(in crate::orchestrator::driver) owed: BTreeMap<String, ShipOwed>,
    opened: Option<String>,
    beat_at: Option<Instant>,
    /// How long a session idles before it parks.
    pub(super) idle: Duration,
    /// How often the lease and the queue are beaten and read back.
    pub(super) beat_every: Duration,
    /// The least a queue held at the usage limit waits (`limit.rs`).
    pub(super) limit_floor: Duration,
}

impl Experts {
    /// No sessions, and the changes the state file says the store is still owed (`ship_runs.rs`).
    pub(super) fn with_owed(owed: BTreeMap<String, ShipOwed>) -> Self {
        Self {
            owed,
            ..Self::default()
        }
    }
}

impl Default for Experts {
    fn default() -> Self {
        Self {
            queue: None,
            sessions: BTreeMap::new(),
            shipped: Vec::new(),
            owed: BTreeMap::new(),
            opened: None,
            beat_at: None,
            idle: expert::IDLE,
            beat_every: Duration::from_millis(BEAT_MS),
            limit_floor: limit::AT_LEAST,
        }
    }
}

/// What a pass needs from the project and the machine.
struct Cx<'a> {
    id: &'a str,
    project: &'a ProjectConfig,
    client: &'a Client,
    repo: u64,
    token: &'a str,
    agent: ClaimNextAgent,
    host: &'a Arc<dyn ExpertHost>,
    sessions: Sessions,
    queue: Arc<ExpertQueue<OwnedQueue>>,
    /// Which engines are at their usage limit, and until when.
    limits: crate::orchestrator::fresh_start::EngineLimits,
    limit_floor: Duration,
    /// Whether this project's item loop can take a shipped item over (`claim` mode).
    ships: bool,
    /// The store socket said the queue changed (`facts_changed` naming `expert_queue`): the beat is due now.
    woke: bool,
}

impl Cx<'_> {
    fn want(&self, item: &str) -> Want {
        Want {
            project: self.id.to_string(),
            item: item.to_string(),
            chore: false,
            last_lane: None,
            fix_queued: false,
        }
    }

    fn holder(&self, item: &str) -> Holder {
        Holder::Item {
            project: self.id.to_string(),
            item: item.to_string(),
        }
    }

    /// A line for the item's history; one that doesn't get there is logged, the session goes on.
    async fn note(&self, id: &str, lease: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if let Err(e) = self.client.report(self.repo, Some(self.token), id, lease, text).await {
            tracing::warn!("{}: expert {id}: the history line didn't reach the store: {e}", self.id);
        }
    }

    /// Lets the item go and skips it, for good (`HostedWork.skip`).
    async fn drop_item(&self, id: &str, lease: &str) -> Result<(), String> {
        let t = Some(self.token);
        self.client
            .release(self.repo, t, id, lease)
            .await
            .map_err(|e| format!("The backlog store wouldn't let {id} go: {e}"))?;
        match self.client.skip(self.repo, t, id).await {
            Err(e) if !e.is_conflict() => Err(format!("The backlog store wouldn't skip {id}: {e}")),
            _ => Ok(()),
        }
    }
}

fn iso(ms: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

fn driven<'a>(ex: &'a mut Experts, item: &str, by: &str) -> Result<&'a mut Session, String> {
    let s = (ex.sessions.get_mut(item)).ok_or_else(|| format!("There's no expert session {item} here."))?;
    match expert::driver_refusal(&s.started_by, by) {
        Some(why) => Err(why),
        None => Ok(s),
    }
}

impl ProjectLoop {
    /// How many expert turns are running here now.
    pub(super) fn experting(&self) -> u32 {
        self.experts.sessions.values().filter(|s| s.turn.is_some()).count() as u32
    }

    /// The expert sessions, as the snapshot's `items` lists them.
    pub(super) fn expert_items(&self) -> Vec<HeldItem> {
        (self.experts.sessions.iter())
            .map(|(id, s)| {
                let mut expert = json!({ "startedBy": s.started_by, "detail": s.detail.as_str() });
                if let Some(ask) = &s.ask {
                    expert["ask"] = ask.to_json();
                }
                let waiting: Vec<Value> = (s.queue.iter())
                    .map(|q| json!({ "id": q.id, "text": q.text, "at": iso(q.at) }))
                    .collect();
                if !waiting.is_empty() {
                    expert["queue"] = json!(waiting);
                }
                if let Some(plan) = s.plan.as_ref().filter(|_| s.detail == Detail::Handed) {
                    expert["plan"] = json!(plan);
                }
                if let Some(work) = &s.work {
                    expert["work"] = work.to_json();
                }
                HeldItem {
                    id: id.clone(),
                    title: s.title.clone(),
                    state: "building".into(),
                    lane: s.lane,
                    branch: s.branch.clone(),
                    expert: Some(expert),
                    ..Default::default()
                }
            })
            .collect()
    }

    async fn expert_store(repo: Option<u64>, shared: &Shared<'_>) -> Option<(u64, String)> {
        let repo = repo?;
        match shared.links.store_token().await {
            Ok(Some(token)) => Some((repo, token)),
            _ => None,
        }
    }

    /// Carries out the expert commands queued for this project, then moves each session on: finished turns are
    /// taken in, the next queued message of each idle session starts, idle sessions park, and once a minute the
    /// leases are beaten and the queue read back.
    pub(super) async fn expert_pass(&mut self, pool: &mut LanePool, host: &Arc<dyn ExpertHost>, shared: &Shared<'_>) {
        let out = &shared.links.item_commands;
        let cmds = out.take_experts(&self.id);
        if cmds.is_empty() && self.experts.sessions.is_empty() && self.experts.owed.is_empty() {
            return;
        }
        let answer = |cmd: &ExpertCommand, r: Result<Value, String>| {
            let msg = match r {
                Ok(Value::Null) => json!({ "type": "ack", "id": cmd.command_id }),
                Ok(a) => json!({ "type": "ack", "id": cmd.command_id, "answer": a }),
                Err(reason) => json!({ "type": "nack", "id": cmd.command_id, "reason": reason }),
            };
            out.answer(&self.id, msg);
        };
        let store = if shared.stopping {
            None
        } else {
            Self::expert_store(self.repo, shared).await
        };
        let Some((repo, token)) = store else {
            for cmd in &cmds {
                answer(cmd, Err("This agent can't reach the backlog store right now.".into()));
            }
            return;
        };
        let mut ex = std::mem::take(&mut self.experts);
        let queue = ex
            .queue
            .get_or_insert_with(|| {
                let ops = OwnedQueue {
                    client: self.client.clone(),
                    repo_id: repo,
                    token: token.clone(),
                };
                Arc::new(ExpertQueue::new(ops, shared.links.machine.clone()))
            })
            .clone();
        let cx = Cx {
            id: &self.id,
            project: &self.project,
            client: &self.client,
            repo,
            token: &token,
            agent: claim_agent(shared.links, &self.project),
            host,
            sessions: self.live.sessions().clone(),
            queue,
            limits: shared.engine_limits.clone(),
            limit_floor: ex.limit_floor,
            ships: self.takes_tasks,
            woke: shared.links.work.take(&self.id, crate::work_signal::Wake::ExpertQueue),
        };
        let mut saved = ex.owed.clone();
        ship_runs::retry(&cx, &mut ex, pool).await;
        ship_runs::save(&mut self.store, &ex, &mut saved);
        for cmd in &cmds {
            let mut r = command(&cx, &mut ex, pool, cmd).await;
            if let Some(item) = ex.opened.take() {
                // The change is open: the record is saved before the store hears of it.
                ship_runs::save(&mut self.store, &ex, &mut saved);
                r = ship_runs::settle(&cx, &mut ex, pool, &item).await.map(|()| Value::Null);
                ship_runs::save(&mut self.store, &ex, &mut saved);
            }
            answer(cmd, r);
        }
        reap(&cx, &mut ex).await;
        work::poll(&cx, &mut ex).await;
        helpers::poll(&cx, &mut ex, pool).await;
        for (id, s) in &mut ex.sessions {
            steer::poll(&cx, id, s).await;
        }
        beat(&cx, &mut ex).await;
        start_turns(&cx, &mut ex, pool).await;
        park_idle(&cx, &mut ex, pool).await;
        for shipped in std::mem::take(&mut ex.shipped) {
            self.adopt_shipped(shipped);
        }
        self.experts = ex;
        self.publish_experts();
    }

    /// Puts the sessions into the snapshot's `items` now, rather than at the next full pass.
    fn publish_experts(&self) {
        let mut items = (self.live.snapshot().projects.get(&self.id)).map_or_else(Vec::new, |p| p.items.clone());
        items.retain(|i| i.expert.is_none());
        items.extend(self.expert_items());
        self.live.set_items(&self.id, items);
    }
}

async fn command(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool, cmd: &ExpertCommand) -> Result<Value, String> {
    match &cmd.action {
        ExpertAction::Start { request, continue_from } => {
            start(cx, ex, pool, &cmd.by, request, continue_from.as_deref()).await
        }
        ExpertAction::Say {
            item,
            text,
            pictures,
            now,
        } => {
            let s = driven(ex, item, &cmd.by)?;
            say(cx, pool, s, item, text, pictures, *now).await?;
            Ok(Value::Null)
        }
        ExpertAction::Stop { item } => {
            let s = driven(ex, item, &cmd.by)?;
            s.stopped = true;
            if let Some(t) = &s.turn {
                t.stop.notify_one();
            }
            Ok(Value::Null)
        }
        ExpertAction::End { item, outcome } => end(cx, ex, pool, &cmd.by, item, outcome).await,
        ExpertAction::Helper { item, task, lane } => {
            let s = driven(ex, item, &cmd.by)?;
            let helper = helpers::start(cx, item, s, task, *lane).await?;
            Ok(json!({ "helper": helper }))
        }
        ExpertAction::History { item, before, limit } => session_history(cx, ex, &cmd.by, item, *before, *limit).await,
    }
}

/// `session_history`: any member may ask, but a continued session's Claude Code conversation is its starter's.
async fn session_history(
    cx: &Cx<'_>,
    ex: &Experts,
    by: &str,
    item: &str,
    before: Option<i64>,
    limit: Option<usize>,
) -> Result<Value, String> {
    let s = (ex.sessions.get(item)).ok_or("That's not an expert session.")?;
    let from = cx
        .queue
        .continued_from(item)
        .filter(|_| by == s.started_by)
        .map(|f| f.session_id);
    let (earlier, own) = cx
        .host
        .history(cx.project, item, s.session_id.as_deref(), from.as_deref())
        .await;
    // The running turn is `watch`'s: its meta line is the last, so it goes.
    let own = if s.turn.is_some() {
        &own[..own.len().saturating_sub(1)]
    } else {
        &own[..]
    };
    let all: Vec<history::Turn> = earlier.iter().chain(own).cloned().collect();
    Ok(history::answer(
        &all,
        history::Pick {
            before,
            limit,
            running: false,
        },
    ))
}

/// Queues the person's message for the session; with `now` the running turn is cut and the message goes next.
async fn say(
    cx: &Cx<'_>,
    pool: &LanePool,
    s: &mut Session,
    item: &str,
    text: &str,
    pictures: &[String],
    now: bool,
) -> Result<(), String> {
    if let Some(plan) = s.plan.as_ref().filter(|_| s.detail == Detail::Handed) {
        return Err(expert::handed_refusal(plan));
    }
    // A parked session takes the added lane first: with another session in it the message is refused, as it is there.
    if s.lane.is_none() && !pool.added_free() {
        return Err(expert::ADDED_LANE_TAKEN.into());
    }
    let row = (cx.queue.add(item, text, &s.started_by, pictures, true).await).map_err(|e| format!("{e}"))?;
    s.queue.push(row.clone());
    s.stopped = false;
    // The person is here, so what waited at the usage limit tries again with their message.
    s.limit_until = None;
    if now && let Some(t) = &s.turn {
        // The person's word goes first: the turn is cut and their message is the next one.
        t.stop.notify_one();
    } else if !now {
        // A turn running takes it as it works; any other waits for its turn (`steer.rs`).
        steer::try_send(cx, item, s, &row).await;
    }
    Ok(())
}

async fn start(
    cx: &Cx<'_>,
    ex: &mut Experts,
    pool: &mut LanePool,
    by: &str,
    request: &str,
    continue_from: Option<&str>,
) -> Result<Value, String> {
    // Checked before anything is added: a Claude Code session that's gone is refused.
    let from = continue_from.map(|id| cx.host.continuing(id)).transpose()?;
    // A Claude Code session carried on already: its open session takes the message, rather than a second starting.
    if let Some(open) = from
        .as_ref()
        .and_then(|f| carry::open_session(cx, ex, by, &f.session_id))
    {
        let s = driven(ex, &open, by)?;
        say(cx, pool, s, &open, request, &[], false).await?;
        return Ok(json!({ "itemId": open }));
    }
    let title = from
        .as_ref()
        .map_or_else(|| expert::title(request), |f| f.title.clone());
    let mut added = None;
    for n in 1..=expert::ID_TRIES {
        let id = expert::id_for(&title, n);
        if ex.sessions.contains_key(&id) {
            continue;
        }
        let item = ExpertItem {
            id: id.clone(),
            title: title.clone(),
            remaining: request.to_string(),
        };
        match (cx.client.expert_add(cx.repo, Some(cx.token), &item, by, &cx.agent)).await {
            Ok(a) => {
                added = Some((id, a));
                break;
            }
            Err(e) if e.is_conflict() => {}
            Err(e) => return Err(format!("The backlog store refused the new session: {e}")),
        }
    }
    let (id, added) = added.ok_or("Couldn't find a free id for the session.")?;
    // The added lane (one a computer): with another session in it this one starts parked, its request not sent.
    let lane = pool.claim_added(cx.holder(&id));
    let mut queue = Vec::new();
    if let Some(lane) = lane {
        match cx.queue.add(&id, request, by, &[], false).await {
            Ok(first) => queue.push(first),
            Err(e) => {
                pool.release(lane, &cx.holder(&id));
                let _ = cx.drop_item(&id, &added.token).await;
                return Err(format!("{e}"));
            }
        }
    } else {
        let why = format!(
            "{} The session is parked and your request wasn't sent: send it again to try.",
            expert::ADDED_LANE_TAKEN
        );
        cx.note(&id, &added.token, &why).await;
    }
    let mut session = Session {
        started_by: by.to_string(),
        title: title.clone(),
        branch: expert::branch(&id, &title),
        lease: added.token,
        lane,
        session_id: None,
        prepared: false,
        from_branch: false,
        queue,
        consumed: BTreeSet::new(),
        turn: None,
        detail: if lane.is_some() {
            Detail::Working
        } else {
            Detail::Parked
        },
        ask: None,
        idle_since: Instant::now(),
        stopped: false,
        mid_step: false,
        handover_from: None,
        plan: None,
        item: added.item.clone(),
        request: request.to_string(),
        talk: Vec::new(),
        work: None,
        reading: None,
        prs_at: None,
        helpers: 0,
        helping: Vec::new(),
        results: Vec::new(),
        tell_now: false,
        limit_until: None,
    };
    if let Some(from) = &from {
        carry::carry_on(cx, &mut session, &id, from).await;
    }
    ex.sessions.insert(id.clone(), session);
    Ok(json!({ "itemId": id }))
}

async fn end(
    cx: &Cx<'_>,
    ex: &mut Experts,
    pool: &mut LanePool,
    by: &str,
    item: &str,
    outcome: &str,
) -> Result<Value, String> {
    if !matches!(outcome, "ship" | "park" | "discard" | "backlog") {
        return Err(format!("{outcome} isn't a way to end a session."));
    }
    let s = driven(ex, item, by)?;
    stop_turn(cx, item, s).await;
    helpers::stop_all(cx, pool, item, s).await;
    if outcome == "ship" && !cx.ships {
        let owed = ship_runs::open(cx, pool, item, s).await?;
        ex.owed.insert(item.to_string(), owed);
        ex.opened = Some(item.to_string());
        return Ok(Value::Null);
    }
    if outcome == "ship" {
        let shipped = ship::ship(cx, pool, item, s).await?;
        ex.sessions.remove(item);
        ex.shipped.push(shipped);
        return Ok(Value::Null);
    }
    if outcome == "park" {
        return park(cx, pool, item, s).await.map(|()| Value::Null);
    }
    if outcome == "backlog" {
        return backlog::hand_to_backlog(cx, pool, item, s).await.map(|()| Value::Null);
    }
    // Discard: the queue and the item go from the store, then what the lane and the host hold.
    cx.queue.clear(item).await;
    cx.drop_item(item, &s.lease).await?;
    cx.host.discard(cx.project, s.lane, &s.branch).await;
    if let Some(lane) = s.lane {
        pool.release(lane, &cx.holder(item));
    }
    ex.sessions.remove(item);
    Ok(Value::Null)
}

/// Pushes the branch and frees the lane. The push failing keeps the lane. The lease stays: the session is only
/// asleep, and its next message takes a lane again.
async fn park(cx: &Cx<'_>, pool: &mut LanePool, item: &str, s: &mut Session) -> Result<(), String> {
    helpers::stop_all(cx, pool, item, s).await;
    if let Some(lane) = s.lane {
        (cx.host.park(cx.project, lane, &s.branch).await)
            .map_err(|e| format!("Couldn't push the branch, so the session keeps its lane: {e}"))?;
        pool.release(lane, &cx.holder(item));
        s.lane = None;
    }
    s.detail = Detail::Parked;
    s.plan = None;
    s.ask = None;
    s.prepared = false;
    s.from_branch = true;
    s.session_id = None;
    Ok(())
}

/// Cuts the session's running turn and takes in how it ended.
async fn stop_turn(cx: &Cx<'_>, item: &str, s: &mut Session) {
    if let Some(mut turn) = s.turn.take() {
        turn.stop.notify_one();
        let end = join(turn.task).await;
        if let Some(mut st) = turn.steering.take() {
            steer::take_in(cx, item, s, &mut st).await;
        }
        finish(cx, item, s, turn.ids, end).await;
    }
}

async fn join(task: JoinHandle<TurnEnd>) -> TurnEnd {
    task.await
        .unwrap_or_else(|e| TurnEnd::Failed(format!("the turn crashed: {e}")))
}

/// A turn is over: its rows are read, and its answer goes to the item's history.
async fn finish(cx: &Cx<'_>, item: &str, s: &mut Session, ids: Vec<String>, end: TurnEnd) {
    cx.queue.done(item, &ids).await;
    s.consumed.extend(ids);
    work::read_after_turn(cx, s);
    let limited = matches!(end, TurnEnd::Limited(_));
    s.idle_since = Instant::now();
    s.detail = Detail::Idle;
    s.ask = None;
    match end {
        TurnEnd::Done { session_id, reply } => {
            s.session_id = Some(session_id);
            s.handover_from = None;
            s.mid_step = false;
            s.from_branch = false;
            let said = expert::answer_line(&reply);
            if !said.is_empty() {
                s.talk.push(said.clone());
            }
            cx.note(item, &s.lease, &said).await;
            if reply.ask.is_some() {
                s.detail = Detail::Asking;
            }
            s.ask = reply.ask;
            // The helpers it asked for, once its summary is in the history; one refused says why there.
            for h in reply.helpers {
                if let Err(why) = helpers::start(cx, item, s, &h.task, h.lane).await {
                    cx.note(item, &s.lease, &format!("A helper wasn't started: {why}"))
                        .await;
                }
            }
        }
        TurnEnd::Stopped => s.mid_step = true,
        TurnEnd::Failed(why) | TurnEnd::Limited(why) => {
            if s.session_id.is_none() {
                s.prepared = false;
            }
            cx.note(item, &s.lease, &format!("The turn failed: {why}")).await;
            if limited {
                limit::hold(cx, item, s).await;
            }
        }
    }
}

/// Takes in the turns that have finished.
async fn reap(cx: &Cx<'_>, ex: &mut Experts) {
    for (id, s) in &mut ex.sessions {
        if s.turn.as_ref().is_some_and(|t| t.task.is_finished())
            && let Some(mut turn) = s.turn.take()
        {
            let end = join(turn.task).await;
            if let Some(mut st) = turn.steering.take() {
                steer::take_in(cx, id, s, &mut st).await;
            }
            finish(cx, id, s, turn.ids, end).await;
        }
    }
}

/// Starts the oldest queued message of each session that is between turns, on its lane (taking one again after a
/// park). A message another computer took, or the person cancelled, is dropped.
async fn start_turns(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool) {
    for (id, s) in &mut ex.sessions {
        if s.turn.is_some() || s.detail == Detail::Handed {
            continue;
        }
        if s.queue.is_empty() {
            // Nothing the person said waits: what helpers found is told to a session that is idle.
            helpers::tell(cx, id, s);
            continue;
        }
        if s.stopped || limit::held(s) {
            continue;
        }
        let lane = match s.lane {
            Some(lane) => lane,
            None => match pool.claim_added(cx.holder(id)) {
                Some(lane) => {
                    s.lane = Some(lane);
                    lane
                }
                None => continue,
            },
        };
        let mut got = None;
        while !s.queue.is_empty() {
            let first = s.queue.remove(0);
            if let Some(mut rows) = cx.queue.take(id, &[first]).await {
                got = rows.pop();
                break;
            }
        }
        let Some(m) = got else { continue };
        let saved = match fetch_pictures(cx, &m).await {
            Ok(saved) => saved,
            Err(why) => {
                // The row was taken: it is read and gone, so say so rather than leave the person waiting.
                cx.queue.done(id, std::slice::from_ref(&m.id)).await;
                s.consumed.insert(m.id.clone());
                cx.note(
                    id,
                    &s.lease,
                    &format!("{why} Your message wasn't sent: send it again to try."),
                )
                .await;
                continue;
            }
        };
        s.talk.push(format!("The person: {}", m.text));
        let text = expert::with_images(&m.text, &saved.block());
        let message = if s.handover_from.is_some() && s.session_id.is_none() {
            carry::first_message(cx, s, text).await
        } else if s.mid_step {
            format!("{}\n\n{text}", expert::MID_STEP)
        } else {
            text
        };
        let message = helpers::with_results(s, message);
        begin_turn(cx, id, s, lane, message, saved, vec![m.id]);
    }
}

/// Runs a turn of `message` in `lane`: the session is working until it ends. `ids` are the queue rows it reads.
fn begin_turn(
    cx: &Cx<'_>,
    id: &str,
    s: &mut Session,
    lane: u32,
    message: String,
    pictures: Pictures,
    ids: Vec<String>,
) {
    let stop = Arc::new(Notify::new());
    let (steering, input) = steer::open();
    let job = ExpertJob {
        project: cx.project.clone(),
        item: id.to_string(),
        branch: s.branch.clone(),
        lane,
        session_id: s.session_id.clone(),
        message,
        pictures,
        prepare: !s.prepared,
        from_branch: s.from_branch,
        stop: stop.clone(),
        sessions: cx.sessions.clone(),
        steer: Some(input),
    };
    s.prepared = true;
    s.detail = Detail::Working;
    s.ask = None;
    tracing::info!("{}: expert {id}: running a turn on lane {lane}", cx.id);
    s.turn = Some(Turn {
        task: tokio::spawn(cx.host.turn(job)),
        stop,
        ids,
        steering: Some(steering),
    });
}

/// Fetches the pictures a queued message carries into a folder of their own, named for the row and made unique by
/// this process and a count: the temp folder is shared, so another process (or a run of the same row again) must
/// never be handed this turn's folder, which is removed when the turn ends.
async fn fetch_pictures(cx: &Cx<'_>, m: &Queued) -> Result<Pictures, String> {
    static TURNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name: String = (m.id.chars())
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    let n = TURNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = pictures::folder().join(format!("expert-{}-{n}-{name}", std::process::id()));
    pictures::fetch_own(cx.client, cx.repo, Some(cx.token), &m.picture_ids, &dir).await
}

/// Parks the sessions that have sat idle for the idle time, with nothing queued.
async fn park_idle(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool) {
    let idle = ex.idle;
    for (id, s) in &mut ex.sessions {
        let waiting = s.turn.is_none() && s.queue.is_empty() && matches!(s.detail, Detail::Idle | Detail::Asking);
        if waiting && s.lane.is_some() && s.idle_since.elapsed() >= idle {
            tracing::info!("{}: expert {id}: idle, parking", cx.id);
            if let Err(e) = park(cx, pool, id, s).await {
                tracing::warn!("{}: expert {id}: {e}", cx.id);
                s.idle_since = Instant::now();
            }
        }
    }
}

/// Every [`Experts::beat_every`]: beats the queue's claims and each lease, and reads the store's queue back. A row the
/// store no longer lists was cancelled; one it lists with other text was edited; one this session doesn't know was
/// written elsewhere (the site) and joins the queue in the order it was written. A push from the store socket makes it
/// due now (`woke`); with the socket down it is the minute's beat alone.
async fn beat(cx: &Cx<'_>, ex: &mut Experts) {
    if !cx.woke && ex.beat_at.is_some_and(|t| t.elapsed() < ex.beat_every) {
        return;
    }
    ex.beat_at = Some(Instant::now());
    if cx.queue.holding() {
        cx.queue.beat().await;
    }
    for (id, s) in &ex.sessions {
        if let Err(e) = cx.client.lease_beat(cx.repo, Some(cx.token), id, &s.lease).await {
            tracing::warn!("{}: expert {id}: the lease beat didn't reach the store: {e}", cx.id);
        }
    }
    let Some(rows) = cx.queue.pull().await else { return };
    for (id, s) in &mut ex.sessions {
        let listed: &[Queued] = rows.get(id).map_or(&[], Vec::as_slice);
        s.queue
            .retain(|q| is_local(&q.id) || listed.iter().any(|r| r.id == q.id));
        for r in listed {
            let running = s.turn.as_ref().is_some_and(|t| t.ids.contains(&r.id));
            if running || s.consumed.contains(&r.id) {
                continue;
            }
            match s.queue.iter_mut().find(|q| q.id == r.id) {
                Some(q) => q.text.clone_from(&r.text),
                None => s.queue.push(r.clone()),
            }
        }
        s.queue.sort_by_key(|q| q.at);
    }
}
