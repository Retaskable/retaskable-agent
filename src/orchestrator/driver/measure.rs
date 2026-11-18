//! The measurement events (agent-telemetry.md section 3.4): what a trial is read from, written by the project's loop
//! at the point each thing happens. Each is a one-line call from where it happens; the work of building the event is here.
//! `session_end` for every role, `merged`, `draft_landed`, `chore_start`/`chore_end`, `run_start`/`run_end` and `needs_person`.
use std::collections::{BTreeSet, HashMap};

use super::project::ProjectLoop;
use crate::backlog::runs::{RunKind, RunOutcome, RunPiece};
use crate::item_lifecycle::ItemState;
use crate::telemetry::measure::{clip, reason_code, run_ref};
use crate::telemetry::{Event, event, now_ms};

/// What the loop remembers between the two ends of a measured thing.
#[derive(Debug, Default)]
pub(super) struct Measure {
    /// Items whose change was found merged when the agent came back to them, until their `merged` is written.
    away: BTreeSet<String>,
    /// Items completed because nothing was left to build, until their chore (if one) ends.
    nothing_left: BTreeSet<String>,
    /// Chores that began here, with when and which kind.
    chores: HashMap<String, (i64, String)>,
    /// Runs that began, by token: the kind, the piece, when, and the chore's kind.
    runs: HashMap<String, Seen>,
}

#[derive(Debug)]
struct Seen {
    kind: RunKind,
    piece: String,
    at: i64,
    chore: Option<String>,
}

/// A run's kind as the store names it.
fn kind_name(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Plan => "plan",
        RunKind::Build => "build",
        RunKind::Fix => "fix",
        RunKind::Watch => "watch",
        RunKind::Merge => "merge",
        RunKind::Look => "look",
    }
}

/// The kind of chore a piece is, when it is one.
pub(super) fn chore_kind(piece: &RunPiece) -> Option<String> {
    piece.other.get("chore")?.get("kind")?.as_str().map(String::from)
}

/// Seen runs older than this are forgotten (a run cut by the store's `stop` may never report its end).
const FORGET_MS: i64 = 24 * 60 * 60_000;

impl ProjectLoop {
    /// Writes the `session_end` of every engine session that ended since the last pass, whatever its role.
    pub(super) fn write_session_events(&mut self) {
        for ended in self.live.sessions().take_ended_events(&self.id) {
            self.events.add_in(&self.id, Event::SessionEnd(ended));
        }
    }

    /// An item or run needs a person: one event, with a short code to group by and the words.
    pub(super) fn measure_needs_person(&mut self, item: &str, from: &str, text: &str, run: Option<&str>) {
        self.events.add_in(
            &self.id,
            Event::NeedsPerson(event::NeedsPerson {
                item: Some(item.to_string()),
                reason: Some(reason_code(text).to_string()),
                text: Some(clip(text)),
                from: Some(from.to_string()).filter(|f| !f.is_empty()),
                run: run.map(String::from),
                ..Default::default()
            }),
        );
    }

    /// `item`'s change merged while the agent was away from it: its `merged` says so.
    pub(super) fn measure_away(&mut self, item: &str) {
        self.run_loop.measure.away.insert(item.to_string());
    }

    /// `item` was completed with nothing left to build.
    pub(super) fn measure_nothing_left(&mut self, item: &str) {
        self.run_loop.measure.nothing_left.insert(item.to_string());
    }

    /// A claimed item's slice merged (the PR number and merge commit as the record has them).
    pub(super) fn measure_item_merged(&mut self, item: &str, commit: Option<&str>) {
        let record = self.records.get(item);
        let pr = record.and_then(|r| r.flow.pr_number);
        let how = if self.run_loop.measure.away.remove(item) {
            "away"
        } else if record
            .and_then(|r| r.flow.land.as_deref())
            .is_some_and(|l| l == "auto_merge" || l == "queue")
        {
            "auto_merge"
        } else {
            "merge"
        };
        let Some(pr) = pr else { return };
        self.events.add_in(
            &self.id,
            Event::Merged(event::Merged {
                item: Some(item.to_string()),
                pr: Some(pr),
                chore: Some(self.chores.contains_key(item)),
                commit: commit.map(String::from),
                how: Some(how.into()),
                ..Default::default()
            }),
        );
    }

    /// A merge run's `merged`, with the merge commit its end read (`result.commit`) and how the project lands changes.
    pub(super) fn measure_piece_merged(&self, mut merged: event::Merged, result: &serde_json::Value) -> event::Merged {
        merged.commit = result.get("commit").and_then(|c| c.as_str()).map(String::from);
        let auto = matches!(
            crate::drafts::land::workflow(&self.project).land.as_str(),
            "auto_merge" | "queue"
        );
        merged.how = Some(if auto { "auto_merge" } else { "merge" }.into());
        merged
    }

    /// A plan's pull request merged here and its items are on their way to the store.
    pub(super) fn measure_draft_landed(&mut self, draft: &str, pr: Option<u64>, items: u64) {
        self.events.add_in(
            &self.id,
            Event::DraftLanded(event::DraftLanded {
                draft: Some(draft.to_string()),
                pr,
                items: Some(items),
                ..Default::default()
            }),
        );
    }

    /// A claim-mode chore item was taken.
    pub(super) fn measure_chore_start(&mut self, id: &str, kind: &str) {
        self.run_loop
            .measure
            .chores
            .insert(id.to_string(), (now_ms(), kind.to_string()));
        self.write_chore_start(id, kind, "claim");
    }

    fn write_chore_start(&mut self, id: &str, kind: &str, mode: &str) {
        self.events.add_in(
            &self.id,
            Event::ChoreStart(event::ChoreStart {
                item: Some(id.to_string()),
                chore: Some(kind.to_string()),
                mode: Some(mode.into()),
                ..Default::default()
            }),
        );
    }

    /// A claim-mode chore item is let go of: it merged, found nothing left, stopped for a person, or failed.
    pub(super) fn measure_chore_end(&mut self, id: &str) {
        let outcome = match self.runs.get(id).map(|r| r.state) {
            Some(ItemState::Merged | ItemState::Done) if self.run_loop.measure.nothing_left.remove(id) => {
                "nothing_left"
            }
            Some(ItemState::Merged | ItemState::Done) => "merged",
            Some(ItemState::NeedsHuman | ItemState::NeedsClarification) => "stopped",
            _ => "failed",
        };
        let kind = self.chores.get(id).map(|c| c.kind.clone());
        self.write_chore_end(id, kind, "claim", outcome);
    }

    fn write_chore_end(&mut self, id: &str, kind: Option<String>, mode: &str, outcome: &str) {
        let started = self.run_loop.measure.chores.remove(id);
        let kind = started.as_ref().map(|(_, k)| k.clone()).or(kind);
        self.events.add_in(
            &self.id,
            Event::ChoreEnd(event::ChoreEnd {
                item: Some(id.to_string()),
                chore: kind,
                mode: Some(mode.into()),
                outcome: Some(outcome.into()),
                ms: started.map(|(at, _)| (now_ms() - at).max(0) as f64),
                ..Default::default()
            }),
        );
    }

    /// A run began on `piece` (a chore's kind when the piece is one).
    pub(super) fn measure_run_started(&mut self, token: &str, kind: RunKind, piece: &str, chore: Option<String>) {
        let now = now_ms();
        let seen = &mut self.run_loop.measure.runs;
        seen.retain(|_, s| now - s.at < FORGET_MS);
        seen.insert(
            token.to_string(),
            Seen {
                kind,
                piece: piece.to_string(),
                at: now,
                chore: chore.clone(),
            },
        );
        self.events.add_in(
            &self.id,
            Event::RunStart(event::RunStart {
                run: Some(run_ref(token)),
                run_kind: Some(kind_name(kind).into()),
                piece: Some(piece.to_string()),
                ..Default::default()
            }),
        );
        if let (Some(chore), RunKind::Build) = (chore, kind) {
            self.measure_run_chore_start(piece, &chore);
        }
    }

    fn measure_run_chore_start(&mut self, piece: &str, chore: &str) {
        let known = self.run_loop.measure.chores.contains_key(piece);
        if !known {
            self.run_loop
                .measure
                .chores
                .insert(piece.to_string(), (now_ms(), chore.to_string()));
            self.write_chore_start(piece, chore, "runs");
        }
    }

    /// A run ended with `outcome`: its `run_end`, a `needs_person` when it asks for one, and the chore's end when it was
    /// the chore's last run.
    pub(super) fn measure_run_ended(&mut self, token: &str, outcome: &RunOutcome) {
        let Some(seen) = self.run_loop.measure.runs.remove(token) else {
            return;
        };
        let why = match outcome {
            RunOutcome::Failed(why) => Some(clip(why)),
            _ => None,
        };
        let name = outcome_name(outcome);
        self.events.add_in(
            &self.id,
            Event::RunEnd(event::RunEnd {
                run: Some(run_ref(token)),
                run_kind: Some(kind_name(seen.kind).into()),
                piece: Some(seen.piece.clone()),
                outcome: Some(name.into()),
                ms: Some((now_ms() - seen.at).max(0) as f64),
                why: why.clone().filter(|w| !w.is_empty()),
                ..Default::default()
            }),
        );
        let text = match outcome {
            RunOutcome::Failed(why) => Some(why.as_str()),
            RunOutcome::Questions => Some("the run ended with questions for a person"),
            RunOutcome::PersonSteps => Some("only steps a person can do are left"),
            _ => None,
        };
        if let Some(text) = text {
            let run = run_ref(token);
            self.measure_needs_person(&seen.piece, kind_name(seen.kind), text, Some(&run));
        }
        if seen.chore.is_some() {
            let ended = match (outcome, seen.kind) {
                (RunOutcome::Merged, RunKind::Merge) => Some("merged"),
                (RunOutcome::NothingLeft, _) => Some("nothing_left"),
                (RunOutcome::Failed(_), _) => Some("failed"),
                (RunOutcome::Questions | RunOutcome::PersonSteps | RunOutcome::Conflict, _) => Some("stopped"),
                _ => None,
            };
            if let Some(ended) = ended {
                self.write_chore_end(&seen.piece, seen.chore.clone(), "runs", ended);
            }
        }
    }
}

/// An outcome as the store names it.
fn outcome_name(outcome: &RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Interrupted => "interrupted",
        RunOutcome::Failed(_) => "failed",
        RunOutcome::Planned => "planned",
        RunOutcome::PrOpened => "pr_opened",
        RunOutcome::Questions => "questions",
        RunOutcome::PersonSteps => "person_steps",
        RunOutcome::NothingLeft => "nothing_left",
        RunOutcome::Green => "green",
        RunOutcome::Red => "red",
        RunOutcome::Merged => "merged",
        RunOutcome::Updated => "updated",
        RunOutcome::Conflict => "conflict",
        RunOutcome::Looked => "looked",
        RunOutcome::Limited { .. } => "limited",
    }
}
