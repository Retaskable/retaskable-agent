//! Looking at a stopped item (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md bullets 1, 16, 18 and
//! 20): while a project's `triage` is on, each `needs_human` stop that `schedule::wants_triage` holds for gets one
//! read-only triager session, started through the [`SessionStarter`] when a session slot is free, and its end comes
//! back on the outcome channel like any session's. Every finding is carried out as `human`: its reason is added to the
//! item's, with a `person_steps` answer's steps listed under it. A look that fails is tried again 10 minutes later,
//! then 20 (or once the usage limit lifts); on the third failure the reason says Claude couldn't look into it. A look
//! that hit its budget isn't tried again. The record holds the marks, so a restart never looks at a stop twice.
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{OutcomeSender, ProjectLoop, SessionStarter, Shared};
use crate::config::OnOff;
use crate::engines::{EngineId, Role, engine_for, program_of};
use crate::lane_pool::LanePool;
use crate::orchestrator::fresh_start::LIMIT_PAUSE;
use crate::telemetry::{Event, event};
use crate::triage::record::needs_human_facts;
use crate::triage::schedule::{MAX_TRIAGES, claude_on_it, wants_triage};
use crate::triage::{TriageContext, TriageDecision};

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
            let facts = needs_human_facts(r, false, self.triaging.contains(item_id));
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
    pub(super) async fn triage_pass(
        &mut self,
        pool: &LanePool,
        starter: &dyn SessionStarter,
        room: &mut u32,
        shared: &Shared<'_>,
    ) {
        // Behind the `triage-session` release flag: Off is today's behaviour, no looks and no `claudeOnIt`.
        self.triage_flag = self.flags.on("triage-session").await;
        if !self.triage_flag
            || self.project.triage != OnOff::On
            || shared.stopping
            || super::project::signed_out(shared)
        {
            return;
        }
        let engine = program_of(engine_for(
            &shared.links.engine_choice,
            self.project.engines.as_ref(),
            Role::Triager,
            None,
        ));
        if shared.engine_limits.is_limited(engine, Instant::now()) {
            return;
        }
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
            let Some(context) = self.triage_context(&id, std::path::Path::new("")) else {
                continue;
            };
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
    pub(super) fn triage_ended(&mut self, item_id: &str, lane: u32, end: TriageEnd, shared: &mut Shared<'_>) {
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
            TriageEnd::Found { reason, steps, .. } => {
                // Every decision is left to a person for now (`human`), so nothing is lost: the reason and any
                // steps are listed under the item's.
                let mut text = format!("Claude looked into it: {reason}");
                for (n, step) in steps.iter().enumerate() {
                    text.push_str(&format!("\n{}. {step}", n + 1));
                }
                self.look_found(item_id, &text);
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
