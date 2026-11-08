//! How work the loop started comes back to it: a session's end over the outcome channel ([`SessionOutcome`],
//! settled on the next wake), a step's end through the project's `Running` set, and what either means for the
//! item's lane and lease (freed, parked, or given back to the store, with the releases and parks the store
//! hasn't taken yet sent again later: the TS agent's `unreleased`).
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;

use super::{Launch, Links, Machine, ProjectLoop, SessionStarter, Shared};
use crate::engines::signed_out_text;
use crate::item_lifecycle::{ImplementerOutcome, ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::fresh_start::{FreshStart, LIMIT_PAUSE, picking_up_prompt};
use crate::orchestrator::steps::{StepRequest, has_step, needs_lane, state_name};
use crate::telemetry::{Event, event};

/// How long an own failure before any session ran (the TS agent's `ownFailure`) pauses claiming of new
/// work for every project on this computer (section 7.3): a problem with this computer, not the item, so
/// hammering the store for more work would likely just fail the same way.
pub(super) const CLAIM_PAUSE: Duration = Duration::from_secs(30 * 60);

/// How long a session that found Claude Code signed out (section 7.3's `claudeSignedOut`) blocks new or
/// resumed Claude session starts on this computer: long enough that a person has time to sign back in
/// before the next retry, short enough that work resumes soon after they do.
pub(super) const SIGNED_OUT_PAUSE: Duration = Duration::from_secs(60);

/// How long after a watchdog kill the same session is resumed.
pub(super) const SILENCE_RESUME_DELAY: Duration = Duration::from_secs(60);

/// Watchdog kills in a row that are resumed; the next one goes to a person.
const MAX_SILENCE_RESUMES: u32 = 3;

/// Own failures in a row (a session that wouldn't start, or a hand-back) that are retried or handed back; the
/// next one goes to a person.
const MAX_OWN_FAILURES: u32 = 3;

/// A give-back's note, cut to this many characters (agent/07.5-what-a-claim-and-a-give-back-send.md).
const MAX_NOTE_CHARS: usize = 4000;

/// How a started session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionResult {
    /// The implementer exited normally (it finished, synced main, or asked questions).
    Implementer(ImplementerOutcome),
    /// A failure before any session ran (lane prep, or the session wouldn't even start): this computer's own
    /// problem, not the item's (the TS `ownFailure`), so the item is given back to the store's queue and
    /// this project's claiming pauses ([`hand_back`](ProjectLoop::hand_back)). The text says why.
    Failed(String),
    /// A session ran but ended in a way that needs a person: it crashed, exited without an answer, gave up,
    /// found nothing left to build, left steps only a person can do, or answered with a status this agent
    /// doesn't know — any of these may have left real work behind, so the run goes to `needs_human` instead
    /// of back to the queue, freeing its lane and keeping its lease. The text says why.
    NeedsHuman(String),
    /// Something outside the session — a restart or Quit (07.4-stopping-and-restarting.md) — cut its
    /// process tree and killed it before it answered, not the implementer giving up. The run stays
    /// `implementing` with its lane and lease exactly as they were (no release, no park, nothing posted to
    /// the store): the next restart readopts it and resumes the same session by id.
    Interrupted,
    /// The session's process wouldn't start (the text says why), so nothing ran. Behind the
    /// `own-failure-handling` flag the run keeps its lane and lease, claiming isn't paused, and the loop starts
    /// the session again after [`SILENCE_RESUME_DELAY`], up to [`MAX_OWN_FAILURES`] times; the next failure
    /// goes to `needs_human`. With the flag Off it is a [`NeedsHuman`](Self::NeedsHuman).
    NotStarted(String),
    /// The watchdog killed the session for printing nothing (the text says why). Behind the
    /// `own-failure-handling` flag the run stays `implementing` with its lane and lease, nothing is posted to
    /// the store, and the loop resumes the session by id after [`SILENCE_RESUME_DELAY`]; the 4th kill in a row
    /// goes to `needs_human`. With the flag Off it is a [`Failed`](Self::Failed).
    Silenced(String),
    /// The session's engine hit its usage limit, so the run needs a person like any session that ended without
    /// answering; the computer also remembers the engine as limited until `resets_in` from now (or
    /// [`LIMIT_PAUSE`]), so a later resume can start fresh on another engine (engines/03-choosing-the-engine.md
    /// section 3.5).
    Limited { why: String, resets_in: Option<Duration> },
    /// A triager session ended (06.9); `outcome.lane` is the item's lane, 0 when it had none.
    Triage(super::TriageEnd),
    /// A triager's second look ended (06.9 bullet 3).
    SecondLook(super::SecondLookEnd),
}

/// A session's end, sent back to the loop that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOutcome {
    pub project: String,
    pub item_id: String,
    pub lane: u32,
    pub result: SessionResult,
    /// The session's own size as of its last turn (06.2-implementer.md "A session that's grown
    /// starts fresh"), kept on the item's record for a later resume to weigh against
    /// `max_resume_context_tokens`. `None` when the engine never reported one (any engine but
    /// Claude, or a session that ended before its first turn).
    pub context_tokens: Option<u64>,
    /// The session's latest usage reading, if any, saved as `state.usage` once settled
    /// (docs/specs/agent/07-orchestrator-loop.md section 7.1).
    pub usage: Option<Value>,
    /// Lines for the item's timeline (the code gates' note, 06.2-implementer.md), logged as the session's
    /// end settles.
    pub notes: Vec<String>,
    /// The id of a new session this one carried on in (a send-back to one that had grown too large, or a
    /// restart after running out of turns), saved instead of the old one. `None` when the session kept its id.
    pub session_id: Option<String>,
}

/// The sending end of the outcome channel: cloned into every [`StartRequest`](super::StartRequest).
#[derive(Debug, Clone)]
pub struct OutcomeSender(pub(super) mpsc::UnboundedSender<SessionOutcome>);

impl OutcomeSender {
    /// A sender and the receiving end of its channel.
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<SessionOutcome>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    /// Reports a session's end. A loop that has stopped hears nothing, which is fine: nothing is left to settle.
    pub fn send(&self, outcome: SessionOutcome) {
        let _ = self.0.send(outcome);
    }
}

impl PartialEq for OutcomeSender {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }
}

impl Eq for OutcomeSender {}

impl ProjectLoop {
    /// Gives an item's lane and lease back: its run is dropped, the lane freed (`lane_freed`), and the lease
    /// moved to `unreleased` (saved before anything is sent, so a restart never strands the claim) and
    /// released at once. `state` is the item's state for the `lease` event and `why` the reason, as in the
    /// TS agent's `lane_freed` (`<state>: <note>`). An own failure before any session ran, so the error goes
    /// in the local event log (not the item's: it isn't the item's fault) and claiming pauses, for every
    /// project on this computer, for [`CLAIM_PAUSE`] (section 7.3, [`Machine::claim_paused_until`]), restarting
    /// cleared.
    pub(super) async fn hand_back(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        state: &str,
        why: &str,
        shared: &mut Shared<'_>,
    ) {
        self.runs.remove(item_id);
        self.items.remove(item_id);
        self.free_lane(pool, item_id, state, why);
        *shared.claim_paused_until = Some(Instant::now() + CLAIM_PAUSE);
        self.events.add_in(
            &self.id,
            Event::Error(event::Error {
                item: Some(item_id.to_string()),
                r#where: Some("start".into()),
                error: Some(why.to_string()),
                ..Default::default()
            }),
        );
        if let Some(token) = self.leases.remove(item_id) {
            let note = crate::text::truncate(why, MAX_NOTE_CHARS);
            self.unreleased.insert(item_id.to_string(), (token, note));
            self.save_leases();
            self.events.add_in(
                &self.id,
                Event::Lease(event::Lease {
                    item: Some(item_id.to_string()),
                    change: Some("released".into()),
                    state: Some(state.into()),
                    ..Default::default()
                }),
            );
            self.flush_owed(shared.links).await;
        }
    }

    /// Lets go of the lease while the item waits for `waits_for` (the TS `park`): the lane is freed, the run
    /// kept, and the lease moved to `unparked` (saved first) and parked at once. The store keeps the item as
    /// this computer's, so no other computer is handed it.
    async fn park(&mut self, pool: &mut LanePool, item_id: &str, state: &str, waits_for: &str, links: &Links) {
        self.free_lane(pool, item_id, state, "waiting");
        if let Some(token) = self.leases.remove(item_id) {
            self.unparked
                .insert(item_id.to_string(), (token, waits_for.into(), state.into()));
            self.save_leases();
            self.flush_owed(links).await;
        }
    }

    /// Frees the item's lane, if it holds one, and writes `lane_freed` (`<state>: <note>`).
    pub(super) fn free_lane(&mut self, pool: &mut LanePool, item_id: &str, state: &str, why: &str) {
        let holder = Holder::Item {
            project: self.id.clone(),
            item: item_id.to_string(),
        };
        if let Some(lane) = pool.lane_of(&self.id, item_id)
            && pool.release(lane, &holder)
        {
            // The lane's latest user is the one a Play names.
            self.last_lane.retain(|_, k| *k != lane);
            self.last_lane.insert(item_id.to_string(), lane);
            self.events.add_in(
                &self.id,
                Event::LaneFreed(event::LaneFreed {
                    item: Some(item_id.to_string()),
                    lane: Some(lane),
                    why: Some(format!("{state}: {why}")),
                    ..Default::default()
                }),
            );
        }
    }

    /// Sends the releases and parks still owed. One the store refuses (or can't reach) stays listed for
    /// the next pass; `stale_lease` means the lease was gone already, so it counts as done.
    pub(super) async fn flush_owed(&mut self, links: &Links) {
        let Some(repo) = self
            .repo
            .filter(|_| !self.unreleased.is_empty() || !self.unparked.is_empty() || !self.unskipped.is_empty())
        else {
            return;
        };
        let Ok(Some(token)) = links.keychain.get(&links.backlog_url) else {
            return;
        };
        for (id, (lease, note)) in self.unreleased.clone() {
            // report then release (section 7.5): the note lands in the item's history while the
            // lease still holds. A refusal of either keeps both for the next pass.
            match self.client.report(repo, Some(&token), &id, &lease, &note).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("stale_lease") => {}
                Err(e) => {
                    self.store_error("report", &e);
                    continue;
                }
            }
            match self.client.release(repo, Some(&token), &id, &lease).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("stale_lease") => {}
                Err(e) => {
                    self.store_error("release", &e);
                    continue;
                }
            }
            self.unreleased.remove(&id);
        }
        // A skip goes after its release; a `conflict` means the item is skipped (or finished) already.
        for id in self.unskipped.clone() {
            if self.unreleased.contains_key(&id) {
                continue;
            }
            match self.client.skip(repo, Some(&token), &id).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("conflict") => {}
                Err(e) => {
                    self.store_error("skip", &e);
                    continue;
                }
            }
            self.unskipped.remove(&id);
        }
        for (id, (lease, waits_for, state)) in self.unparked.clone() {
            match self.client.park(repo, Some(&token), &id, &lease, &waits_for).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("stale_lease") => {}
                Err(e) => {
                    self.store_error("park", &e);
                    continue;
                }
            }
            self.unparked.remove(&id);
            self.events.add_in(
                &self.id,
                Event::Lease(event::Lease {
                    item: Some(id),
                    change: Some("parked".into()),
                    state: Some(state),
                    ..Default::default()
                }),
            );
        }
        self.save_leases();
    }

    /// What an item's run moving from `from` to its state now means, after a session or a step ended
    /// (`note` says which): an `item_state` event when it moved; questions for a person free the lane and
    /// park; any other state that runs nothing in a lane (`needs_lane`) frees it and keeps the lease (as the
    /// TS agent does for `needs_human` with no PR, and until a merge lands for the rest).
    async fn moved(&mut self, pool: &mut LanePool, item: &str, from: ItemState, note: &str, links: &Links) {
        let Some(to) = self.runs.get(item).map(|r| r.state) else {
            return;
        };
        if to == ItemState::NeedsHuman && from != to {
            // A session's own reason is its note; a step's is fixed by the state it escalated from.
            let reason = if from == ItemState::Implementing {
                note.to_string()
            } else {
                super::triage_facts::step_stop_reason(from).to_string()
            };
            let slice = self.runs.get(item).map_or(0, |r| r.slice);
            if let Some(record) = self.records.get_mut(item) {
                record.new_stop(state_name(from), &reason, slice);
                record.state = "needs_human".into();
            }
        }
        if from != to {
            self.events.add_in(
                &self.id,
                Event::ItemState(event::ItemState {
                    item: Some(item.to_string()),
                    from: Some(state_name(from).into()),
                    to: Some(state_name(to).into()),
                    claude_on_it: (to == ItemState::NeedsHuman
                        && self.triage_flag
                        && self.project.triage == crate::config::OnOff::On)
                        .then(|| self.claude_on_it(item)),
                    ..Default::default()
                }),
            );
        }
        match to {
            ItemState::NeedsClarification => self.park(pool, item, state_name(to), "answer", links).await,
            to if !needs_lane(to) => self.free_lane(pool, item, state_name(to), note),
            _ => {}
        }
        self.refresh_record(pool, item);
    }

    /// Settles one session's end on the run (`ItemRun::implementer_finished`, plain code), then what its new
    /// state means for the lane and lease ([`moved`](Self::moved)). A failed session gives its lane and lease
    /// back. A finished slice goes on to `checking`, keeping both, and the loop's next wake steps it.
    async fn settle(&mut self, outcome: SessionOutcome, pool: &mut LanePool, shared: &mut Shared<'_>) {
        if let SessionResult::Triage(end) = &outcome.result {
            self.triage_ended(&outcome.item_id, outcome.lane, end.clone(), pool, shared)
                .await;
            return;
        }
        if let SessionResult::SecondLook(end) = &outcome.result {
            self.second_look_ended(&outcome.item_id, end.clone(), pool, shared)
                .await;
            return;
        }
        let item = outcome.item_id;
        let lane = outcome.lane;
        let Some(run) = self.runs.get_mut(&item).filter(|r| r.state == ItemState::Implementing) else {
            tracing::warn!(
                "{}: a session ended for {item}, which isn't implementing here; ignored",
                self.id
            );
            return;
        };
        if let Some(record) = self.records.get_mut(&item) {
            // A fresh session replaces the old one: its id is saved, and its size starts from none.
            if let Some(id) = outcome.session_id.clone() {
                record.impl_session_id = Some(id);
                record.impl_context_tokens = None;
            }
            if let Some(tokens) = outcome.context_tokens {
                record.impl_context_tokens = Some(tokens);
            }
        }
        if let Some(id) = outcome.session_id.clone()
            && let Some(record) = self.records.get_mut(&item)
        {
            record.impl_session_id = Some(id);
        }
        if let Some(usage) = outcome.usage.clone().filter(crate::tiers::is_five_hour_reading) {
            crate::tiers::note_usage(&usage);
            self.store.set_usage(usage);
            if let Err(e) = self.store.save() {
                tracing::warn!("{}: saving the usage reading failed: {e:#}", self.id);
            }
        }
        for note in &outcome.notes {
            self.events.add_in(
                &self.id,
                Event::ItemState(event::ItemState {
                    item: Some(item.clone()),
                    from: Some("implementing".into()),
                    to: Some("implementing".into()),
                    note: Some(note.clone()),
                    lane: Some(lane),
                    ..Default::default()
                }),
            );
        }
        self.events.add_in(
            &self.id,
            Event::SessionEnd(event::SessionEnd {
                item: Some(item.clone()),
                lane: Some(outcome.lane),
                role: Some("implementer".into()),
                engine: Some("claude".into()),
                ok: Some(matches!(outcome.result, SessionResult::Implementer(_))),
                subtype: match outcome.result {
                    SessionResult::Interrupted => Some("interrupted".into()),
                    SessionResult::Silenced(_) => Some("killed_for_silence".into()),
                    _ => None,
                },
                ..Default::default()
            }),
        );
        if let SessionResult::Limited { resets_in, .. } = &outcome.result
            && let Some(record) = self.records.get(&item)
        {
            let until = Instant::now() + resets_in.unwrap_or(LIMIT_PAUSE);
            shared.engine_limits.hit(record.impl_session_engine, until);
        }
        let mut result = outcome.result;
        let flag_on = self.flags.on("own-failure-handling").await;
        if let SessionResult::NotStarted(why) = result {
            let count = self.own_failures.get(&item).copied().unwrap_or(0);
            result = if !flag_on {
                SessionResult::NeedsHuman(why)
            } else if count < MAX_OWN_FAILURES {
                self.own_failures.insert(item.clone(), count + 1);
                self.silenced
                    .insert(item.clone(), Instant::now() + SILENCE_RESUME_DELAY);
                if let Some(record) = self.records.get_mut(&item) {
                    record.not_started = true;
                }
                SessionResult::NotStarted(why)
            } else {
                SessionResult::NeedsHuman(why)
            };
        } else {
            self.fresh_launched.remove(&item);
            if let Some(record) = self.records.get_mut(&item) {
                record.not_started = false;
            }
        }
        if flag_on && let SessionResult::Failed(why) = result {
            let count = self.own_failures.get(&item).copied().unwrap_or(0);
            result = if count < MAX_OWN_FAILURES {
                self.own_failures.insert(item.clone(), count + 1);
                SessionResult::Failed(why)
            } else {
                SessionResult::NeedsHuman(why)
            };
        }
        // A session that started and answered (or needs a person) ends the run of own failures.
        if matches!(
            result,
            SessionResult::Implementer(_) | SessionResult::NeedsHuman(_) | SessionResult::Limited { .. }
        ) {
            self.own_failures.remove(&item);
        }
        self.store.set_own_failures(&self.own_failures);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the own-failure count failed: {e:#}", self.id);
        }
        if let SessionResult::Silenced(why) = result {
            let record = self.records.get_mut(&item);
            result = match record {
                // Off: today's behaviour, the item goes back to the queue.
                _ if !self.flags.on("own-failure-handling").await => SessionResult::Failed(why),
                Some(record) if record.silence_kills < MAX_SILENCE_RESUMES => {
                    record.silence_kills += 1;
                    self.silenced
                        .insert(item.clone(), Instant::now() + SILENCE_RESUME_DELAY);
                    SessionResult::Silenced(why)
                }
                record => {
                    if let Some(record) = record {
                        record.silence_kills = 0;
                    }
                    SessionResult::NeedsHuman(why)
                }
            };
        } else if let Some(record) = self.records.get_mut(&item) {
            // Any answer ends the run of kills.
            record.silence_kills = 0;
        }
        match result {
            // A cut session (07.4): the run stays implementing, with its lane and lease untouched, and
            // nothing is posted to the store. The next restart readopts it and resumes the same session.
            SessionResult::Silenced(_) | SessionResult::NotStarted(_) => self.save_leases(),
            SessionResult::Interrupted | SessionResult::Triage(_) | SessionResult::SecondLook(_) => {}
            SessionResult::Failed(why) => self.hand_back(pool, &item, "queued", &why, shared).await,
            SessionResult::NeedsHuman(why) | SessionResult::Limited { why, .. } => {
                run.escalate();
                // Claude's sign-in running out is this computer's problem, not the item's (section 7.3):
                // the item still needs a person (it may have left real work behind), but no other item
                // should start a session here until someone signs back in.
                if signed_out_text(Some(&why)) {
                    *shared.claude_signed_out_until = Some(Instant::now() + SIGNED_OUT_PAUSE);
                }
                self.moved(pool, &item, ItemState::Implementing, &why, shared.links)
                    .await;
            }
            SessionResult::Implementer(result) => {
                run.implementer_finished(result, &self.project);
                let note = if run.state == ItemState::NeedsHuman {
                    "The session kept asking what a person had already answered, or the item reached its slice limit."
                        .to_string()
                } else {
                    format!("the session on lane {} ended", outcome.lane)
                };
                self.moved(pool, &item, ItemState::Implementing, &note, shared.links)
                    .await;
            }
        }
    }

    /// Moves the items whose watchdog-kill delay has passed to `to_resume`, up to `room` of them (the
    /// session slots left); [`resume_sessions`](Self::resume_sessions) then resumes each by its saved id.
    pub(super) fn release_silenced(&mut self, mut room: u32) {
        let now = Instant::now();
        let due: Vec<String> = (self.silenced.iter())
            .filter(|(_, at)| **at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            if room == 0 {
                break;
            }
            room -= 1;
            self.silenced.remove(&id);
            if self.runs.get(&id).is_some_and(|r| r.state == ItemState::Implementing) {
                self.to_resume.push(id);
            }
        }
    }

    /// A fresh session on the item's branch for `reason`: the picking-up prompt around `follow_up`, with the
    /// reason's line added to the item's history.
    pub(super) fn fresh_launch(&mut self, item_id: &str, lane: u32, reason: FreshStart, follow_up: &str) -> Launch {
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item_id.to_string()),
                from: Some("implementing".into()),
                to: Some("implementing".into()),
                note: Some(reason.history_line()),
                lane: Some(lane),
                ..Default::default()
            }),
        );
        let has_pr = self.items.get(item_id).is_some_and(|i| i.pr_url.is_some());
        let prompt = picking_up_prompt(has_pr, &self.project.base_branch, follow_up);
        match reason {
            FreshStart::EngineLimited { to, .. } => Launch::ContinuingOn(prompt, to),
            _ => Launch::Continuing(prompt),
        }
    }

    /// Takes in the steps that ended: each item's run as its step left it, then what its new state means
    /// ([`moved`](Self::moved)). A step that panicked is an `error` event and leaves the run as it was.
    pub(super) async fn take_finished(&mut self, pool: &mut LanePool, links: &Links) {
        for (item, after) in self.running.finished() {
            let Some(run) = self.runs.get_mut(&item) else { continue };
            let Some(after) = after else {
                tracing::error!("{}: a step for {item} panicked", self.id);
                let error = event::Error {
                    item: Some(item),
                    r#where: Some("step".into()),
                    error: Some("the step panicked".into()),
                    ..Default::default()
                };
                self.events.add_in(&self.id, Event::Error(error));
                continue;
            };
            let before = std::mem::replace(run, after);
            self.moved(pool, &item, before.state, "the step ended", links).await;
        }
    }

    /// Runs the next step of every item that has one and isn't already running one (the TS agent's
    /// `for (const it of items) this.advance(it.id)`), each as its own task. An `implementing` item's step
    /// is its session, started once (`apply_starts`) and settled when it reports its end, so it's skipped here.
    pub(super) fn advance(&mut self, pool: &LanePool, starter: &dyn SessionStarter) {
        let ready: Vec<(String, ItemRun)> = (self.runs.iter())
            .filter(|(id, run)| has_own_step(run.state) && !self.running.busy(id))
            .map(|(id, run)| (id.clone(), *run))
            .collect();
        for (item_id, run) in ready {
            let start = event::StepStart {
                item: Some(item_id.clone()),
                step: Some(state_name(run.state).into()),
                ..Default::default()
            };
            let request = StepRequest {
                project: self.id.clone(),
                lane: pool.lane_of(&self.id, &item_id),
                token: self.leases.get(&item_id).cloned().unwrap_or_default(),
                item_id: item_id.clone(),
                run,
            };
            // No step built for this state yet: the item waits where it is, and nothing is logged.
            let Some(step) = starter.advance(request) else { continue };
            self.events.add_in(&self.id, Event::StepStart(start));
            self.running.spawn(item_id, step);
        }
    }
}

/// The states [`SessionStarter::advance`] is asked to step: the TS step table, less `implementing`.
fn has_own_step(state: ItemState) -> bool {
    has_step(state) && state != ItemState::Implementing
}

impl Machine {
    /// Sends the releases and parks still owed by the `claim` mode projects (once a pass).
    pub(super) async fn flush_all_owed(&mut self) {
        for lp in (self.projects.iter_mut()).filter(|lp| lp.mode == super::OrchestratorMode::Claim) {
            lp.flush_owed(&self.links).await;
        }
    }

    /// Settles the sessions that ended since the last wake.
    pub(super) async fn settle_outcomes(&mut self) {
        while let Ok(outcome) = self.outcomes.try_recv() {
            let stopping = self.stopping();
            match self.projects.iter_mut().find(|lp| lp.id == outcome.project) {
                Some(lp) => {
                    lp.settle(
                        outcome,
                        &mut self.pool,
                        &mut Shared {
                            links: &self.links,
                            claim_paused_until: &mut self.claim_paused_until,
                            claude_signed_out_until: &mut self.claude_signed_out_until,
                            engine_limits: &mut self.engine_limits,
                            stopping,
                        },
                    )
                    .await
                }
                None => tracing::warn!("a session ended for the unknown project {}", outcome.project),
            }
        }
    }
}
