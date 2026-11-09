//! Looking at a stopped item (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md bullets 1, 16, 18 and
//! 20): while a project's `triage` is on, each `needs_human` stop that `schedule::wants_triage` holds for gets one
//! read-only triager session, started through the [`SessionStarter`] when a session slot is free, and its end comes
//! back on the outcome channel like any session's. A `human` finding's reason is added to the
//! item's, with a `person_steps` answer's steps listed under it. A `continue`, `start_over` or `skip` decision is
//! carried out through the button code (`item_commands.rs`), and a `person_steps` one with no PR waits for its steps. A look that fails is tried again 10 minutes later,
//! then 20 (or once the usage limit lifts); on the third failure the reason says Claude couldn't look into it. A look
//! that hit its budget isn't tried again. The record holds the marks, so a restart never looks at a stop twice.
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::start_gate::StartGate;
use super::{OutcomeSender, ProjectLoop, SessionStarter, Shared};
use crate::config::OnOff;
use crate::engines::{EngineId, Role, engine_for, program_of};
use crate::lane_pool::LanePool;
use crate::orchestrator::fresh_start::LIMIT_PAUSE;
use crate::telemetry::{Event, event};
use crate::triage::record::needs_human_facts;
use crate::triage::schedule::{MAX_TRIAGES, claude_on_it, triage_count, wants_triage};
use crate::triage::second_look::{SecondLookResult, resolve};
use crate::triage::{RetaskableBug, TriageButton, TriageContext, TriageDecision, TriageResult};

/// Failed looks at one stop before the item waits for a person with a note.
const MAX_FAILED_LOOKS: u32 = 3;

/// What a triager session needs.
#[derive(Debug, Clone)]
pub struct TriageRequest {
    pub project: String,
    pub item_id: String,
    /// What the triager is given. The starter fills in where the item's transcripts are.
    pub context: TriageContext,
    /// The item's lane when it kept one; else the session runs in the base checkout.
    pub lane: Option<u32>,
    /// The project's latest `state.usage` reading, for the model tier.
    pub usage: Option<Value>,
    /// Where the session reports how it ended.
    pub outcomes: OutcomeSender,
    /// The `engine-placement` release flag: Off, Claude alone runs it.
    pub placement: bool,
    /// With `engine-start-rules` On, the engine to start on when it isn't the triager's chosen one.
    pub engine: Option<EngineId>,
    /// The sessions running now: this one is listed while it runs.
    pub sessions: crate::orchestrator::live::sessions::Sessions,
}

/// What a second-look session needs (06.9 bullet 3): the triager's request, plus its own answer.
#[derive(Debug, Clone)]
pub struct SecondLookRequest {
    pub project: String,
    pub item_id: String,
    pub context: TriageContext,
    pub lane: Option<u32>,
    pub usage: Option<Value>,
    pub outcomes: OutcomeSender,
    pub triager_reason: String,
    pub triager_suggest: TriageButton,
    pub triager_instructions: String,
    /// The `engine-placement` release flag: Off, the second look runs on Claude.
    pub placement: bool,
    /// The sessions running now: this one is listed while it runs.
    pub sessions: crate::orchestrator::live::sessions::Sessions,
}

/// How a second-look session ended: its answer, or `None` when it failed or answered with nothing usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondLookEnd {
    pub agree: Option<(bool, String, String)>,
}

/// A triager's `human` answer waiting on its second look.
#[derive(Debug, Clone)]
pub(super) struct Held {
    reason: String,
    steps: Vec<String>,
    instructions: String,
    suggest: TriageButton,
}

/// Why a look failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookFailure {
    /// A crash, a sign-in refresh or an answer that wasn't usable: tried again later.
    Other,
    /// It hit its budget: not tried again.
    BudgetHit,
    /// The engine's usage limit, lifting in `resets_in` when known.
    Limited { resets_in: Option<Duration> },
}

/// How a triager session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriageEnd {
    Found {
        decision: TriageDecision,
        reason: String,
        steps: Vec<String>,
        /// The next message (`continue`) or notes (`start_over`); empty when there are none.
        instructions: String,
        /// A bug in Retaskable itself the triager named, whatever it decided (06.9 bullet 19).
        bug: Option<RetaskableBug>,
        /// The button a `human` answer offers first.
        suggest: Option<TriageButton>,
    },
    Failed {
        note: String,
        why: LookFailure,
    },
}

/// The item's reason with `text` added under it.
fn add_to_reason(reason: &mut Option<String>, text: &str) {
    *reason = Some(match reason.as_deref().filter(|r| !r.is_empty()) {
        Some(old) => format!("{old}\n\n{text}"),
        None => text.to_string(),
    });
}

impl ProjectLoop {
    /// Whether Claude is looking into `item_id` or about to (`claudeOnIt`).
    pub(super) fn claude_on_it(&self, item_id: &str) -> bool {
        self.records.get(item_id).is_some_and(|r| {
            let facts = needs_human_facts(
                r,
                self.claude_pending.contains(item_id),
                self.triaging.contains(item_id),
            );
            claude_on_it(&facts, self.triage_flag && self.project.triage == OnOff::On)
        })
    }

    /// An `item_state` note about the item's look, with `claudeOnIt` as it stands now.
    pub(super) fn look_note(&mut self, item_id: &str, note: String) {
        let on_it = self.claude_on_it(item_id);
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item_id.to_string()),
                from: Some("needs_human".into()),
                to: Some("needs_human".into()),
                note: Some(note),
                claude_on_it: Some(on_it),
                ..Default::default()
            }),
        );
    }

    /// Starts a triager on each stopped item that wants one while `room` session slots are free. Not while signed
    /// out, stopping or paused (the caller doesn't ask when paused), or the triager's engine is at its limit.
    fn triager_engine(&self, shared: &Shared<'_>) -> EngineId {
        program_of(engine_for(
            &shared.links.engine_choice,
            self.project.engines.as_ref(),
            Role::Triager,
            None,
        ))
    }

    pub(super) async fn triage_pass(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        room: &mut u32,
        shared: &mut Shared<'_>,
    ) {
        self.bug_answers();
        for (id, lane, held) in std::mem::take(&mut self.second_due) {
            let (reason, steps, instructions) = (held.reason.clone(), held.steps.clone(), held.instructions.clone());
            if !self.start_second_look(&id, lane, held, starter) {
                self.decided(
                    &id,
                    TriageDecision::Human,
                    &reason,
                    &steps,
                    &instructions,
                    false,
                    pool,
                    shared,
                )
                .await;
            }
        }
        // Behind the `triage-session` release flag: Off is today's behaviour, no looks and no `claudeOnIt`.
        self.triage_flag = self.flags.on("triage-session").await;
        self.placement_flag = self.flags.on("engine-placement").await;
        if !self.triage_flag || self.project.triage != OnOff::On || shared.stopping {
            return;
        }
        let gate = StartGate::new(self.flags.on("engine-start-rules").await, self, pool, shared);
        let (engine, start_on) = match &gate {
            Some(gate) => match gate.role_start(self, shared, Role::Triager, self.placement_flag) {
                Some(Some(now)) => (now, Some(now)),
                Some(None) => (self.triager_engine(shared), None),
                None => return,
            },
            None => {
                let engine = self.triager_engine(shared);
                if super::project::signed_out(shared) || ProjectLoop::engine_limited(shared, engine) {
                    return;
                }
                (engine, None)
            }
        };
        let now = Utc::now();
        let wanted: Vec<String> = (self.records.iter())
            .filter(|(id, r)| {
                self.leases.contains_key(*id)
                    && !self.triaging.contains(*id)
                    && wants_triage(
                        &needs_human_facts(r, false, false),
                        true,
                        MAX_TRIAGES,
                        r.next_look_at.as_deref(),
                        now,
                    )
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in wanted {
            if *room == 0 {
                break;
            }
            let Some(mut context) = self.triage_context(&id, std::path::Path::new("")) else {
                continue;
            };
            if let Some(link) = shared.links.bugs.get(&self.id) {
                context.known_bugs = link.known();
            }
            let lane = pool.lane_of(&self.id, &id);
            self.triaging.insert(id.clone());
            self.look_note(&id, "Claude is looking into it".into());
            self.events.add_in(
                &self.id,
                Event::SessionStart(event::SessionStart {
                    item: Some(id.clone()),
                    lane,
                    role: Some("triager".into()),
                    engine: Some(engine.to_string()),
                    ..Default::default()
                }),
            );
            let request = TriageRequest {
                project: self.id.clone(),
                item_id: id.clone(),
                context,
                lane,
                usage: self.store.usage(),
                outcomes: self.outcomes.clone(),
                placement: self.placement_flag,
                engine: start_on,
                sessions: self.live.sessions().clone(),
            };
            match starter.triage(request) {
                Ok(()) => *room -= 1,
                Err(why) => {
                    tracing::warn!("{}: the triager for {id} didn't start: {why}", self.id);
                    let note = format!("couldn't look into it ({why})");
                    self.look_ended(&id, note, LookFailure::Other);
                }
            }
        }
    }

    /// A triager's end, back on the outcome channel. An item that a person already moved on from is left alone.
    pub(super) async fn triage_ended(
        &mut self,
        item_id: &str,
        lane: u32,
        end: TriageEnd,
        pool: &mut LanePool,
        shared: &mut Shared<'_>,
    ) {
        self.events.add_in(
            &self.id,
            Event::SessionEnd(event::SessionEnd {
                item: Some(item_id.to_string()),
                lane: Some(lane),
                role: Some("triager".into()),
                ok: Some(matches!(end, TriageEnd::Found { .. })),
                ..Default::default()
            }),
        );
        match end {
            TriageEnd::Found {
                decision,
                reason,
                steps,
                instructions,
                bug,
                suggest,
            } => {
                self.report_bug(item_id, bug, shared);
                if let (TriageDecision::Human, Some(suggest)) = (decision, suggest)
                    && self.records.get(item_id).is_some_and(|r| r.state == "needs_human")
                {
                    let held = Held {
                        reason: reason.clone(),
                        steps: steps.clone(),
                        instructions: instructions.clone(),
                        suggest,
                    };
                    // Started by the next `triage_pass`, which holds the starter; `claudeOnIt` stays on.
                    self.second_due.push((item_id.to_string(), lane, held));
                    return;
                }
                self.decided(item_id, decision, &reason, &steps, &instructions, false, pool, shared)
                    .await;
            }
            TriageEnd::Failed { note, why } => {
                if let LookFailure::Limited { resets_in } = &why {
                    let lifts = resets_in.unwrap_or(LIMIT_PAUSE);
                    shared.engine_limits.hit(EngineId::Claude, Instant::now() + lifts);
                }
                self.look_ended(item_id, note, why);
            }
        }
    }

    /// Starts the top-tier second look at a `human` answer with a suggestion (06.9 bullet 3), keeping the item
    /// `claudeOnIt` meanwhile. `false` when it didn't start: the answer stands as `human`.
    fn start_second_look(&mut self, item_id: &str, lane: u32, held: Held, starter: &dyn SessionStarter) -> bool {
        let Some(context) = self.triage_context(item_id, std::path::Path::new("")) else {
            return false;
        };
        let lane = Some(lane).filter(|l| *l != 0);
        let request = SecondLookRequest {
            project: self.id.clone(),
            item_id: item_id.to_string(),
            context,
            lane,
            usage: self.store.usage(),
            outcomes: self.outcomes.clone(),
            triager_reason: held.reason.clone(),
            triager_suggest: held.suggest,
            triager_instructions: held.instructions.clone(),
            placement: self.placement_flag,
            sessions: self.live.sessions().clone(),
        };
        self.events.add_in(
            &self.id,
            Event::SessionStart(event::SessionStart {
                item: Some(item_id.to_string()),
                lane,
                role: Some("triager".into()),
                ..Default::default()
            }),
        );
        match starter.second_look(request) {
            Ok(()) => {
                self.second_looking.insert(item_id.to_string(), held);
                true
            }
            Err(why) => {
                tracing::warn!("{}: the second look at {item_id} didn't start: {why}", self.id);
                false
            }
        }
    }

    /// A second look's end: folded into the triager's answer with [`resolve`] and carried out like it.
    pub(super) async fn second_look_ended(
        &mut self,
        item_id: &str,
        end: SecondLookEnd,
        pool: &mut LanePool,
        shared: &mut Shared<'_>,
    ) {
        let Some(held) = self.second_looking.remove(item_id) else {
            return;
        };
        self.events.add_in(
            &self.id,
            Event::SessionEnd(event::SessionEnd {
                item: Some(item_id.to_string()),
                role: Some("triager".into()),
                ok: Some(end.agree.is_some()),
                ..Default::default()
            }),
        );
        let triage = TriageResult {
            decision: TriageDecision::Human,
            person_steps: held.steps.clone(),
            reason: held.reason,
            instructions: held.instructions,
            suggest: Some(held.suggest),
            retaskable_bug: None,
        };
        let look = end.agree.map(|(agree, reason, instructions)| SecondLookResult {
            agree,
            reason,
            instructions,
        });
        let r = resolve(&triage, look.as_ref());
        let checked = r.decision != TriageDecision::Human;
        self.decided(
            item_id,
            r.decision,
            &r.reason,
            &held.steps,
            &r.instructions,
            checked,
            pool,
            shared,
        )
        .await;
    }

    /// Carries out the triager's `decision` through the same code as the buttons (06.9 bullet 2). Continue and Start
    /// over are queued for a lane, marked as Claude's; each automatic one counts in `triages` and is never asked
    /// about again. Past the cap, or with a person's command already waiting, it's left for the person.
    #[allow(clippy::too_many_arguments)]
    async fn decided(
        &mut self,
        item_id: &str,
        decision: TriageDecision,
        reason: &str,
        steps: &[String],
        instructions: &str,
        checked: bool,
        pool: &mut LanePool,
        shared: &mut Shared<'_>,
    ) {
        let mut text = format!("Claude looked into it: {reason}");
        for (n, step) in steps.iter().enumerate() {
            text.push_str(&format!("\n{}. {step}", n + 1));
        }
        let Some(record) = self.records.get_mut(item_id).filter(|r| r.state == "needs_human") else {
            self.triaging.remove(item_id);
            return;
        };
        let count = triage_count(record.triages, record.slice);
        let queued = (self.continuing.iter().chain(&self.starting_over)).any(|(id, _)| id == item_id);
        let has_pr = self.items.get(item_id).is_some_and(|i| i.pr_url.is_some());
        let verb = match decision {
            TriageDecision::Continue => "continue",
            TriageDecision::StartOver => "start over",
            TriageDecision::Skip => "skip",
            TriageDecision::PersonSteps if !steps.is_empty() && !has_pr => "wait for your steps",
            _ => "",
        };
        if verb.is_empty() || queued || (verb != "wait for your steps" && count >= MAX_TRIAGES) {
            self.look_found(item_id, &text);
            return;
        }
        self.triaging.remove(item_id);
        record.triaged = true;
        if verb == "wait for your steps" {
            let list: Vec<String> = steps
                .iter()
                .enumerate()
                .map(|(n, s)| format!("{}. {s}", n + 1))
                .collect();
            record.needs_human_reason = Some(format!("Only steps a person can do are left:\n{}", list.join("\n")));
        } else {
            record.triages = Some((record.slice, count + 1));
        }
        let notes = Some(instructions.trim().to_string()).filter(|n| !n.is_empty());
        match decision {
            TriageDecision::Continue => {
                self.claude_pending.insert(item_id.to_string());
                self.continuing.push((item_id.to_string(), notes));
            }
            TriageDecision::StartOver => {
                self.claude_pending.insert(item_id.to_string());
                self.starting_over.push((item_id.to_string(), notes));
            }
            _ => {}
        }
        self.save_leases();
        // After a second look, `reason` already reads "Claude decided to …, checked with a second look: …".
        let line = if checked {
            reason.to_string()
        } else {
            format!("Claude decided to {verb}: {reason}")
        };
        self.look_note(item_id, line);
        if decision == TriageDecision::Skip {
            self.skip_stopped(pool, item_id, true, shared).await;
        }
    }

    /// The look found `text` (already shaped like "Claude looked into it: …").
    fn look_found(&mut self, item_id: &str, text: &str) {
        self.triaging.remove(item_id);
        let Some(record) = self.records.get_mut(item_id).filter(|r| r.state == "needs_human") else {
            return;
        };
        add_to_reason(&mut record.needs_human_reason, text);
        record.triaged = true;
        self.save_leases();
        self.look_note(item_id, text.to_string());
    }

    /// A look that failed with `note`: retried 10 then 20 minutes later or once the usage limit lifts,
    /// given up on at the third failure; a look that hit its budget isn't retried.
    fn look_ended(&mut self, item_id: &str, note: String, why: LookFailure) {
        self.triaging.remove(item_id);
        let Some(record) = self.records.get_mut(item_id).filter(|r| r.state == "needs_human") else {
            return;
        };
        let line = format!("Claude couldn't look into it: {note}");
        let mut retry = None;
        match why {
            LookFailure::BudgetHit => record.triaged = true,
            why => {
                record.failed_looks += 1;
                let mut wait = Duration::from_secs(60 * 10 * u64::from(record.failed_looks));
                if let LookFailure::Limited { resets_in } = why {
                    wait = wait.max(resets_in.unwrap_or(LIMIT_PAUSE));
                }
                if record.failed_looks >= MAX_FAILED_LOOKS {
                    add_to_reason(&mut record.needs_human_reason, &line);
                    record.triaged = true;
                } else {
                    let at: DateTime<Utc> = Utc::now() + wait;
                    record.next_look_at = Some(at.to_rfc3339());
                    retry = Some(wait.as_secs() / 60);
                }
            }
        }
        self.save_leases();
        let line = match retry {
            Some(minutes) => format!("{line} (trying again in {minutes} minutes)"),
            None => line,
        };
        self.look_note(item_id, line);
    }
}
