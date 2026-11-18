//! How work the loop started comes back to it: a session's end over the outcome channel ([`SessionOutcome`],
//! settled on the next wake), a step's end through the project's `Running` set, and what either means for the
//! item's lane and lease (freed, parked, or given back to the store, with the releases and parks the store
//! hasn't taken yet sent again later: the TS agent's `unreleased`).
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;

use super::resume::takeover_in;
use super::{Launch, Links, Machine, ProjectLoop, SessionStarter, Shared};
use crate::engines::EngineId;
use crate::engines::signed_out_text;
use crate::item_lifecycle::{ImplementerOutcome, ItemRun, ItemState};
use crate::lane_pool::{Holder, LanePool};
use crate::orchestrator::fresh_start::{FreshStart, picking_up_prompt};
use crate::orchestrator::steps::{StepRequest, has_step, needs_lane, state_name};
use crate::telemetry::{Event, event};

/// A give-back's note, cut to this many characters (agent/07.5-what-a-claim-and-a-give-back-send.md).
const MAX_NOTE_CHARS: usize = 4000;

/// How long a release the store keeps refusing stays listed before it is dropped:
/// the store has expired the lease by then.
pub(super) const STUCK_RELEASE_SECS: i64 = 24 * 60 * 60;

/// The `needs_human` reason a Cursor implementer stops with after 3 limit hits in a row; the first fetch's
/// give-back (`readopt`) matches it exactly.
pub(super) const CURSOR_LIMIT_STOP: &str = "Cursor's usage limit was hit 3 times in a row";

/// How a started session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionResult {
    /// The implementer exited normally (it finished, synced main, or asked questions).
    Implementer(ImplementerOutcome),
    /// The implementer exited with questions nobody has answered (`needs_clarification`): the questions are kept on the
    /// item's record, and it settles as [`ImplementerOutcome::Questions`] with some still open.
    Asked(Vec<String>),
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
    /// The session's process wouldn't start (the text says why), so nothing ran. The run keeps
    /// its lane and lease, claiming isn't paused, and the loop starts the session again after
    /// the store's `silenceResumeSeconds`, up to the store's `ownFailureRetries` times; the next failure goes to `needs_human`.
    NotStarted(String),
    /// The watchdog killed the session for printing nothing (the text says why). The run stays
    /// `implementing` with its lane and lease, nothing is posted to the store, and the loop resumes the session
    /// by id after the store's `silenceResumeSeconds`; the 4th kill in a row goes to `needs_human`.
    /// The text says why; the second is the `session_end` subtype (`killed_for_silence`, or `cursor_time` for
    /// the Cursor time limit, which is handled the same way).
    Silenced(String, &'static str),
    /// The session's engine hit its usage limit, so the run needs a person like any session that ended without
    /// answering; the computer also remembers the engine as limited until `resets_in` from now (or
    /// the store's `cursorLimitWaitMinutes`), so a later resume can start fresh on another engine (engines/03-choosing-the-engine.md
    /// section 3.5).
    Limited { why: String, resets_in: Option<Duration> },
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
    /// At the weekly cutoff Claude is out for engine fall-over on this computer until the week's reset
    /// ([`EngineLimits::cut_off_for_week`](crate::orchestrator::fresh_start::EngineLimits::cut_off_for_week)).
    pub(super) fn cut_off_for_week(
        id: &str,
        usage: &Value,
        limits: &mut crate::orchestrator::fresh_start::EngineLimits,
    ) {
        if limits.cut_off_for_week(usage) {
            tracing::info!(
                "{}: the weekly limit is at its cutoff, so Claude is out for engine fall-over until it resets",
                id
            );
        }
    }

    /// Gives an item's lane and lease back: its run is dropped, the lane freed (`lane_freed`), and the lease
    /// moved to `unreleased` (saved before anything is sent, so a restart never strands the claim) and
    /// released at once. `state` is the item's state for the `lease` event and `why` the reason, as in the
    /// TS agent's `lane_freed` (`<state>: <note>`). An own failure before any session ran, so the error goes
    /// in the local event log (not the item's: it isn't the item's fault) and is reported in every project's `health`
    /// ([`Machine::own_failure`]) until a session starts well: the store holds the pause, this computer has none.
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
        *shared.own_failure = Some(crate::backlog::pool::OwnFailure {
            at: crate::telemetry::now_ms(),
            why: why.chars().take(300).collect(),
        });
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
            self.list_unreleased(item_id.to_string(), token, note);
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

    /// Gives a Cursor-limited item with nothing pushed back to the shared backlog at once (engines/03 section
    /// 3.2): the run and its record are dropped, the lane freed and the lease moved to
    /// `unreleased` with the note and released. The computer's limit, not an own failure: no claim pause and no
    /// `error` event (Cursor's own stamp in `engine_limits` already keeps Cursor lanes from claiming).
    async fn give_back(&mut self, pool: &mut LanePool, item_id: &str, shared: &mut Shared<'_>) {
        let why = format!(
            "Given back: Cursor's usage limit was hit on {} before anything was pushed",
            shared.links.machine
        );
        self.runs.remove(item_id);
        self.items.remove(item_id);
        self.free_lane(pool, item_id, "queued", &why);
        if let Some(token) = self.leases.remove(item_id) {
            let note = crate::text::truncate(&why, MAX_NOTE_CHARS);
            self.list_unreleased(item_id.to_string(), token, note);
            self.save_leases();
            self.events.add_in(
                &self.id,
                Event::Lease(event::Lease {
                    item: Some(item_id.to_string()),
                    change: Some("released".into()),
                    state: Some("queued".into()),
                    ..Default::default()
                }),
            );
            self.flush_owed(shared.links).await;
        }
    }

    /// Lets go of the lease while the item waits for `waits_for` (the TS `park`): the lane is freed, the run
    /// kept, and the lease moved to `unparked` (saved first) and parked at once. The store keeps the item as
    /// this computer's, so no other computer is handed it.
    pub(super) async fn park(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        state: &str,
        waits_for: &str,
        links: &Links,
    ) {
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
            // As the TS agent's `lane: undefined, lastLane: k`: a record keeps no lane once it holds none.
            if let Some(record) = self.records.get_mut(item_id) {
                record.lane = None;
            }
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

    /// Sends the releases, skips, parks and playtest answers still owed. One the store refuses (or can't reach) stays listed for
    /// the next pass; `stale_lease` means the lease was gone already, so it counts as done.
    pub(super) async fn flush_owed(&mut self, links: &Links) {
        self.resend_progress(links).await;
        self.note_playtests_setting().await;
        let Some(repo) = self.repo.filter(|_| {
            self.release_owed
                || !self.unreleased.is_empty()
                || !self.unparked.is_empty()
                || !self.unskipped.is_empty()
                || !self.unanswered.is_empty()
        }) else {
            return;
        };
        let Ok(Some(token)) = links.store_token().await else {
            return;
        };
        for (id, (lease, note)) in self.unreleased.clone() {
            // The note is trimmed and cut at MAX_NOTE_CHARS, and a blank one sends the release alone.
            let note = crate::text::truncate(note.trim(), MAX_NOTE_CHARS);
            // report then release (section 7.5): the note lands in the item's history while the
            // lease still holds. A refusal of either keeps both for the next pass.
            if !note.is_empty() {
                match self.client.report(repo, Some(&token), &id, &lease, &note).await {
                    Ok(()) => {}
                    Err(e) if e.code() == Some("stale_lease") => {}
                    Err(e) => {
                        self.store_error("report", &e);
                        self.drop_if_stuck(&id).await;
                        continue;
                    }
                }
            }
            match self.client.release(repo, Some(&token), &id, &lease).await {
                Ok(()) => {}
                Err(e) if e.code() == Some("stale_lease") => {}
                Err(e) => {
                    self.store_error("release", &e);
                    self.drop_if_stuck(&id).await;
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
        self.send_owed_answers(repo, &token).await;
        self.send_owed_release(repo, &token).await;
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

    /// Epoch seconds, or the test's clock.
    pub(super) fn now_secs(&self) -> i64 {
        #[cfg(test)]
        if let Some(t) = self.clock {
            return t;
        }
        crate::telemetry::now_secs()
    }

    /// Lists a lease the store hasn't taken back yet; one already listed keeps the time it was first listed.
    pub(super) fn list_unreleased(&mut self, id: String, token: String, note: String) {
        let now = self.now_secs();
        self.unreleased_since.entry(id.clone()).or_insert(now);
        self.unreleased.insert(id, (token, note));
    }

    /// Drops an `unreleased` entry (and the skip owed after it) first listed
    /// [`STUCK_RELEASE_SECS`] or more ago, with one `error` event.
    async fn drop_if_stuck(&mut self, id: &str) {
        let Some(&since) = self.unreleased_since.get(id) else {
            return;
        };
        if self.now_secs() - since < STUCK_RELEASE_SECS {
            return;
        }
        self.unreleased.remove(id);
        self.unskipped.remove(id);
        self.events.add_in(
            &self.id,
            Event::Error(event::Error {
                item: Some(id.to_string()),
                r#where: Some("release".into()),
                error: Some("the release was dropped after 24 hours: the store has expired the lease by then".into()),
                ..Default::default()
            }),
        );
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
                record.new_stop(&reason);
                record.state = "needs_human".into();
            }
            if let Some(held) = self.decisions.get_mut(item) {
                held.now.new_stop(state_name(from), slice);
            }
        }
        if from != to {
            self.events.add_in(
                &self.id,
                Event::ItemState(event::ItemState {
                    item: Some(item.to_string()),
                    from: Some(state_name(from).into()),
                    to: Some(state_name(to).into()),
                    claude_on_it: (to == ItemState::NeedsHuman && self.project.triage == crate::config::OnOff::On)
                        .then(|| self.claude_on_it(item)),
                    ..Default::default()
                }),
            );
        }
        self.report_progress(links, item).await;
        match to {
            // A change waiting on the host or a person's try-out lets go of its lease (`parkFor`), claimed back when the wait ends.
            _ if self.park_for(item).is_some() => self.park_waiting(pool, item, links).await,
            to if !needs_lane(to) => self.free_lane(pool, item, state_name(to), note),
            _ => {}
        }
        self.refresh_record(pool, item);
    }

    /// Settles one session's end on the run (`ItemRun::implementer_finished`, plain code), then what its new
    /// state means for the lane and lease ([`moved`](Self::moved)). A failed session gives its lane and lease
    /// back. A finished slice goes on to `checking`, keeping both, and the loop's next wake steps it.
    async fn settle(&mut self, mut outcome: SessionOutcome, pool: &mut LanePool, shared: &mut Shared<'_>) {
        let item = outcome.item_id.clone();
        self.keep_questions(&item, &mut outcome.result);
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
        if let Some(usage) = &outcome.usage {
            Self::cut_off_for_week(&self.id, usage, shared.engine_limits);
        }
        let week = outcome.usage.clone().filter(crate::tiers::carries_week);
        let five_hour = outcome.usage.clone().filter(crate::tiers::is_usage_reading);
        if week.is_some() || five_hour.is_some() {
            if let Some(usage) = week {
                self.store.set_week_usage(usage);
            }
            if let Some(usage) = five_hour {
                crate::tiers::note_usage(&usage);
                self.store.set_usage(usage);
            }
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
                    SessionResult::Silenced(_, subtype) => Some(subtype.into()),
                    _ => None,
                },
                ..Default::default()
            }),
        );
        // The wait after a limit that gave no reset time is the store's number; unknown, nothing is recorded (nothing starts
        // without the store's numbers, and the next session at the limit says so again).
        if let SessionResult::Limited { resets_in, .. } = &outcome.result
            && let Some(record) = self.records.get(&item)
            && let Some(wait) = resets_in.or(shared.engine_limits.limit_wait())
        {
            shared
                .engine_limits
                .hit(record.impl_session_engine, Instant::now() + wait);
        }
        let mut result = outcome.result;
        // The store's `ownFailureRetries`; unknown, none is retried: the next own failure goes to a person.
        let tuning = self.project.tuning.store;
        let own_retries = tuning.map_or(0, |t| t.own_failure_retries);
        // The store's `silenceResumeSeconds`; only read where a retry is allowed, which needs the numbers.
        let resume_after = tuning.map_or(Duration::ZERO, |t| t.silence_resume());
        if let SessionResult::NotStarted(why) = result {
            let count = self.own_failures.get(&item).copied().unwrap_or(0);
            result = if count < own_retries {
                self.own_failures.insert(item.clone(), count + 1);
                self.silenced.insert(item.clone(), Instant::now() + resume_after);
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
        if let SessionResult::Failed(why) = result {
            let count = self.own_failures.get(&item).copied().unwrap_or(0);
            result = if count < own_retries {
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
            // A session started well: the problem of this computer's own is over, so `health` stops saying so.
            *shared.own_failure = None;
        }
        self.store.set_own_failures(&self.own_failures);
        if let Err(e) = self.store.save() {
            tracing::warn!("{}: saving the own-failure count failed: {e:#}", self.id);
        }
        if let SessionResult::Silenced(why, subtype) = result {
            let record = self.records.get_mut(&item);
            result = match record {
                Some(record) if record.silence_kills < tuning.map_or(0, |t| t.max_silence_resumes) => {
                    record.silence_kills += 1;
                    self.silenced.insert(item.clone(), Instant::now() + resume_after);
                    SessionResult::Silenced(why, subtype)
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
        // Cursor's usage limit (engines/03 section 3.2): waited out like a watchdog kill, three times in a row.
        let mut give_back = false;
        // Another engine this computer runs, and that isn't out, takes it on at once (section 3.5); never from pi.
        let carry_on = match self.records.get(&item) {
            Some(record)
                if matches!(result, SessionResult::Limited { .. }) && record.impl_session_engine != EngineId::Pi =>
            {
                takeover_in(&self.project, pool, shared, record.impl_session_engine)
                    .map(|to| (record.impl_session_engine, to))
            }
            _ => None,
        };
        if let Some((from, to)) = carry_on
            && let Some(record) = self.records.get_mut(&item)
        {
            record.cursor_limit_hits = 0;
            self.silenced.remove(&item);
            self.to_resume.push(item.clone());
            result = SessionResult::Silenced(String::new(), "killed_for_silence");
            self.events.add_in(
                &self.id,
                Event::ItemState(event::ItemState {
                    item: Some(item.clone()),
                    from: Some("implementing".into()),
                    to: Some("implementing".into()),
                    note: Some(format!(
                        "Hit {}'s usage limit; carrying on now with {}",
                        engine_name(from),
                        engine_name(to)
                    )),
                    lane: Some(outcome.lane),
                    ..Default::default()
                }),
            );
        } else if let Some(record) = self.records.get_mut(&item) {
            let cursor_hit =
                matches!(result, SessionResult::Limited { .. }) && record.impl_session_engine == EngineId::Cursor;
            if !cursor_hit {
                record.cursor_limit_hits = 0;
            } else if self.takes_tasks && self.items.get(&item).and_then(|i| i.pr_url.as_ref()).is_none() {
                give_back = true;
            } else if record.cursor_limit_hits < tuning.map_or(0, |t| t.max_cursor_limits)
                && let Some(wait) = shared.engine_limits.limit_wait()
            {
                record.cursor_limit_hits += 1;
                self.silenced.insert(item.clone(), Instant::now() + wait);
                if let SessionResult::Limited { why, .. } = result {
                    result = SessionResult::Silenced(why, "killed_for_silence");
                }
                self.events.add_in(
                    &self.id,
                    Event::ItemState(event::ItemState {
                        item: Some(item.clone()),
                        from: Some("implementing".into()),
                        to: Some("implementing".into()),
                        note: Some(format!(
                            "Hit Cursor's usage limit; trying again in {} minutes",
                            wait.as_secs() / 60
                        )),
                        lane: Some(outcome.lane),
                        ..Default::default()
                    }),
                );
            } else {
                record.cursor_limit_hits = 0;
                result = SessionResult::NeedsHuman(CURSOR_LIMIT_STOP.into());
            }
        }
        if give_back {
            self.give_back(pool, &item, shared).await;
            return;
        }
        match result {
            // A cut session (07.4): the run stays implementing, with its lane and lease untouched, and
            // nothing is posted to the store. The next restart readopts it and resumes the same session.
            SessionResult::Silenced(..) | SessionResult::NotStarted(_) => self.save_leases(),
            SessionResult::Interrupted => {}
            SessionResult::Failed(why) => self.hand_back(pool, &item, "queued", &why, shared).await,
            SessionResult::NeedsHuman(why) | SessionResult::Limited { why, .. } => {
                run.escalate();
                // Claude's sign-in running out is this computer's problem, not the item's (section 7.3):
                // the item still needs a person (it may have left real work behind), but no other item
                // should start a session here until someone signs back in.
                if signed_out_text(Some(&why))
                    && let Some(t) = tuning
                {
                    *shared.claude_signed_out_until = Some(Instant::now() + t.signed_out_retry());
                }
                self.moved(pool, &item, ItemState::Implementing, &why, shared.links)
                    .await;
            }
            // Settled as the questions it is, at the top of `settle` (`keep_questions`).
            SessionResult::Asked(_) => unreachable!("kept as questions before the result is read"),
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
        let has_pr = (self.items.get(item_id).is_some_and(|i| i.pr_url.is_some()))
            || self.records.get(item_id).is_some_and(|r| r.flow.pr_url.is_some());
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
            let outcome = after.as_ref().map_or("panicked", |a| state_name(a.run.state));
            let detail = after.as_ref().map(|a| &a.detail);
            self.step_ended(&item, after.is_some(), outcome, detail);
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
            let merge = after.merge.clone();
            let before = std::mem::replace(run, after.run);
            self.apply_step(&item, before.state, after);
            self.moved(pool, &item, before.state, "the step ended", links).await;
            if let Some(end) = merge {
                self.merge_ended(&item, end, links).await;
            }
        }
    }

    /// Runs the next step of every item that has one and isn't already running one (the TS agent's
    /// `for (const it of items) this.advance(it.id)`), each as its own task. An `implementing` item's step
    /// is its session, started once (`apply_starts`) and settled when it reports its end, so it's skipped here.
    pub(super) fn advance(&mut self, pool: &mut LanePool, starter: &dyn SessionStarter) {
        let ready: Vec<(String, ItemRun)> = (self.runs.iter())
            .filter(|(id, run)| {
                has_own_step(run.state)
                    && !self.running.busy(id)
                    && !self.waiting_to_retry(id)
                    // The store's turn to merge, beaten this pass (`item_merge.rs`).
                    && (run.state != ItemState::Merging || self.turns.may_merge(id))
            })
            .map(|(id, run)| (id.clone(), *run))
            .collect();
        for (item_id, run) in ready {
            // A state that works in a lane, entered from one that holds none (CI passed in `awaiting_ci`): it waits for a lane.
            // The step puts its branch on it.
            if needs_lane(run.state) && pool.lane_of(&self.id, &item_id).is_none() {
                if self.take_lane(pool, &item_id).is_none() {
                    continue;
                }
                self.refresh_record(pool, &item_id);
            }
            let request = StepRequest {
                project: self.id.clone(),
                lane: pool.lane_of(&self.id, &item_id),
                token: self.leases.get(&item_id).cloned().unwrap_or_default(),
                branch: self.records.get(&item_id).and_then(|r| r.branch.clone()),
                flow: self.records.get(&item_id).map(|r| r.flow.clone()).unwrap_or_default(),
                item: self.items.get(&item_id).cloned(),
                config: Some(Box::new(self.project.clone())),
                item_id: item_id.clone(),
                run,
            };
            // No step built for this state yet: the item waits where it is, and nothing is logged.
            let Some(step) = starter.advance(request) else { continue };
            self.step_started(&item_id, &run);
            self.running.spawn(item_id, step);
        }
    }
}

/// The states [`SessionStarter::advance`] is asked to step: the TS step table, less `implementing`.
/// An engine's name as a history line gives it: "Claude", "Cursor".
fn engine_name(engine: EngineId) -> String {
    let name = engine.to_string();
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or(name)
}

fn has_own_step(state: ItemState) -> bool {
    has_step(state) && state != ItemState::Implementing
}

impl Machine {
    /// Sends the releases and parks still owed by the `claim` mode projects (once a pass).
    pub(super) async fn flush_all_owed(&mut self) {
        for lp in (self.projects.iter_mut()).filter(|lp| lp.takes_tasks) {
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
                            local: &self.local,
                            own_failure: &mut self.own_failure,
                            claude_signed_out_until: &mut self.claude_signed_out_until,
                            engine_limits: &mut self.engine_limits,
                            stopping,
                        },
                    )
                    .await;
                }
                None => tracing::warn!("a session ended for the unknown project {}", outcome.project),
            }
        }
    }
}
