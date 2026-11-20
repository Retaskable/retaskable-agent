//! A person's answers to an item's questions (`answer { itemId, answers }`; `answer` in the TS agent's
//! `orchestratorActions.ts`). The hub answers it itself for an item with a piece waiting for an answer, so it reaches
//! this computer only for an item the session asked about (`needs_clarification`, or `needs_human` with its questions
//! still on the card after a push failure). The state moves in plain code, `ItemRun::clarification_answered`: the item
//! goes back to `implementing`, and the implementer is sent the questions with their answers and told to carry on
//! (`Launch::Continuing`, as a failed playtest's is). An item parked while it waited is claimed back by name first
//! (`claim_one`), and the answers go to the shared backlog as a note (`Answered by a person:`), where every
//! computer's card and every later session reads them as decided.
//!
//! The questions come from the session's answer (`SessionResult::Asked`), kept on the item's record and told to the store
//! as `Questions for a person: <q> / <q>`, the way the TS agent's `personNote` says them, so a person sees what they answer.
//! An answer for an item that isn't waiting here is dropped and logged, like Continue's. With no questions on file (a record
//! saved before they were kept) the answers are the person's note, as TS words it.
use super::parking::Claimed;
use super::{Launch, ProjectLoop, SessionStarter, Shared};
use crate::item_lifecycle::ItemState;
use crate::orchestrator::steps::state_name;
use crate::telemetry::{Event, event};

use super::settle::SessionResult;

/// What the store's history note starts with for the questions an item waits on (`QUESTIONS_NOTE`, `shared/personNotes.ts`).
pub(super) const QUESTIONS_NOTE: &str = "Questions for a person: ";
/// The note the answers go to the store as (`ANSWERED_NOTE`).
const ANSWERED_NOTE: &str = "Answered by a person:\n";
/// The store's cap on a history note.
const MAX_NOTE_CHARS: usize = 4000;

/// An answer waiting for a lane: the item and the person's answers (picture paths on the last one).
pub(super) type PendingAnswer = (String, Vec<String>);

/// Each question with its answer, `Q: …\nA: …`, blank-line separated; a question with no answer says so.
pub(super) fn qa_of(questions: &[String], answers: &[String]) -> String {
    let lines: Vec<String> = (questions.iter().enumerate())
        .map(|(i, q)| format!("Q: {q}\nA: {}", answers.get(i).map_or("(no answer)", String::as_str)))
        .collect();
    lines.join("\n\n")
}

/// What the implementer is told when its questions are answered (`followUp` in `answer`).
pub(super) fn follow_up(questions: &[String], answers: &[String]) -> String {
    let note: Vec<&str> = answers
        .iter()
        .map(String::as_str)
        .filter(|a| !a.trim().is_empty())
        .collect();
    let text = if !questions.is_empty() {
        format!("Answers to your questions:\n\n{}", qa_of(questions, answers))
    } else if !note.is_empty() {
        format!("A person's note:\n\n{}", note.join("\n\n"))
    } else {
        "A person read what you said and wants you to carry on.".to_string()
    };
    format!("{text}\n\nContinue with the slice, then return the result.")
}

/// The note that goes to the shared backlog with the answers.
pub(super) fn answered_note(questions: &[String], answers: &[String]) -> String {
    let qa = qa_of(questions, answers);
    let note: Vec<&str> = answers
        .iter()
        .map(String::as_str)
        .filter(|a| !a.trim().is_empty())
        .collect();
    let body = if !qa.is_empty() {
        qa
    } else if !note.is_empty() {
        note.join("\n\n")
    } else {
        "Carry on".to_string()
    };
    format!("{ANSWERED_NOTE}{body}")
}

impl ProjectLoop {
    /// Keeps the questions a session asked on the item's record, and settles the result as the questions it is.
    pub(super) fn keep_questions(&mut self, item: &str, result: &mut SessionResult) {
        let SessionResult::Asked(questions) = result else {
            return;
        };
        if let Some(record) = self.records.get_mut(item) {
            record.flow.questions = std::mem::take(questions);
        }
        *result = SessionResult::Implementer(crate::item_lifecycle::ImplementerOutcome::Questions {
            all_already_answered: false,
        });
    }

    /// What the store is told while the item waits for these answers (`personNote`), cut to the store's cap.
    pub(super) fn questions_note(&self, item: &str) -> Option<String> {
        let record = self.records.get(item).filter(|r| !r.flow.questions.is_empty())?;
        let text = format!("{QUESTIONS_NOTE}{}", record.flow.questions.join(" / "));
        Some(crate::text::truncate(&text, MAX_NOTE_CHARS))
    }

    /// Whether the item is waiting for a person's answer here.
    pub(super) fn awaits_answer(&self, item: &str) -> bool {
        self.runs.get(item).is_some_and(|r| match r.state {
            ItemState::NeedsClarification => true,
            // A push failure can stop an item a second after its session asked: its questions are still on the card.
            ItemState::NeedsHuman => self.records.get(item).is_some_and(|r| !r.flow.questions.is_empty()),
            _ => false,
        })
    }

    /// A parked item that asks nothing (state `needs_clarification`, no questions on its card, not a chore) has nothing for a person to
    /// answer: it is carried on, as an answer with no words is, so it isn't stranded (05-item-lifecycle.md, `needs_clarification`).
    pub(super) fn carry_on_unasked(&mut self) {
        let stranded: Vec<String> = (self.runs.iter())
            .filter(|(id, run)| {
                run.state == ItemState::NeedsClarification
                    && !self.chores.contains_key(*id)
                    && (self.records.get(*id)).is_some_and(|r| r.flow.questions.is_empty())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for item in stranded {
            tracing::info!(
                "{}: {item} is waiting for an answer to no question; carrying on",
                self.id
            );
            self.take_answer(item, Vec::new());
        }
    }

    /// Takes in an answer: queued for a lane when the item is waiting for one here, else dropped and logged.
    pub(super) fn take_answer(&mut self, item: String, answers: Vec<String>) {
        if !self.awaits_answer(&item) || self.answering.iter().any(|(id, _)| *id == item) {
            tracing::info!(
                "{}: dropped an answer for {item}: it isn't waiting for one here",
                self.id
            );
            return;
        }
        self.answering.push((item, answers));
    }

    /// Carries out the answers that can start (`room` is how many more sessions may run). One that must wait for a lane or a
    /// session slot, or whose lease couldn't be claimed back yet, stays queued.
    pub(super) async fn carry_out_answers(
        &mut self,
        pool: &mut crate::lane_pool::LanePool,
        starter: &dyn SessionStarter,
        room: &mut u32,
        shared: &mut Shared<'_>,
    ) {
        for (item, answers) in std::mem::take(&mut self.answering) {
            if !self.resume_answered(pool, &item, &answers, room, starter, shared).await {
                self.answering.push((item, answers));
            }
        }
    }

    /// Carries out one answer. False when it must wait.
    async fn resume_answered(
        &mut self,
        pool: &mut crate::lane_pool::LanePool,
        item: &str,
        answers: &[String],
        room: &mut u32,
        starter: &dyn SessionStarter,
        shared: &mut Shared<'_>,
    ) -> bool {
        if !self.awaits_answer(item) {
            tracing::info!(
                "{}: dropped an answer for {item}: it isn't waiting for one here",
                self.id
            );
            return true;
        }
        if *room == 0 {
            return false;
        }
        // An item that let go of its lease while it waited (`park`: the store keeps it as this computer's) is claimed back by
        // name before anything is done for it; another computer having it now leaves the answer nowhere to go. A park the
        // store hasn't taken yet is sent first.
        if !self.leases.contains_key(item) {
            if self.unparked.contains_key(item) {
                self.flush_owed(shared.links).await;
                if self.unparked.contains_key(item) {
                    return false;
                }
            }
            match self.claim_one(item, shared.links).await {
                Claimed::Held => {}
                Claimed::Gone => return true,
                Claimed::Later => return false,
            }
        }
        let lane_less = pool.lane_of(&self.id, item).is_none();
        let Some(lane) = self.take_lane(pool, item) else {
            return false;
        };
        if self.start_held(pool, shared, lane) {
            self.give_back_lane(pool, item, lane);
            return false;
        }
        let questions = (self.records.get(item))
            .map(|r| r.flow.questions.clone())
            .unwrap_or_default();
        let from = self.runs.get(item).map_or(ItemState::NeedsClarification, |r| r.state);
        let run = self.runs.get_mut(item).expect("checked above");
        run.clarification_answered();
        if let Some(record) = self.records.get_mut(item) {
            record.flow.questions.clear();
        }
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item.to_string()),
                from: Some(state_name(from).into()),
                to: Some("implementing".into()),
                lane: Some(lane),
                note: Some(format!("Answered: {}", answers.join(" / "))),
                ..Default::default()
            }),
        );
        // The answers go to the shared backlog too, so every computer's card shows them. The lease is held again by now.
        if let (Some(repo), Some(lease), Ok(Some(token))) = (
            self.repo,
            self.leases.get(item).cloned(),
            shared.links.store_token().await,
        ) {
            let note = crate::text::truncate(&answered_note(&questions, answers), MAX_NOTE_CHARS);
            if let Err(e) = self.client.report(repo, Some(&token), item, &lease, &note).await {
                self.store_error("report", &e);
            }
        }
        let message = follow_up(&questions, answers);
        // An item with no lane (it waited with its change on the host) starts a new session on its pushed branch.
        let message = if lane_less {
            let has_pr = self.records.get(item).is_some_and(|r| r.flow.pr_url.is_some());
            crate::orchestrator::fresh_start::picking_up_prompt(has_pr, &self.project.base_branch, &message)
        } else {
            message
        };
        *room -= u32::from(
            self.launch(pool, item, lane, Launch::Continuing(message), starter, shared)
                .await,
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn answers_are_paired_with_their_questions_and_a_missing_one_says_so() {
        let qa = qa_of(&s(&["Which?", "Why?"]), &s(&["This one."]));
        assert_eq!(qa, "Q: Which?\nA: This one.\n\nQ: Why?\nA: (no answer)");
    }

    #[test]
    fn the_follow_up_is_ts_words_for_questions_a_note_or_nothing() {
        assert_eq!(
            follow_up(&s(&["Which?"]), &s(&["Blue."])),
            "Answers to your questions:\n\nQ: Which?\nA: Blue.\n\nContinue with the slice, then return the result."
        );
        assert_eq!(
            follow_up(&[], &s(&["Go on", " "])),
            "A person's note:\n\nGo on\n\nContinue with the slice, then return the result."
        );
        assert_eq!(
            follow_up(&[], &[]),
            "A person read what you said and wants you to carry on.\n\nContinue with the slice, then return the result."
        );
    }

    #[test]
    fn the_store_note_is_the_pairs_else_the_note_else_carry_on() {
        assert_eq!(
            answered_note(&s(&["Q?"]), &s(&["A"])),
            "Answered by a person:\nQ: Q?\nA: A"
        );
        assert_eq!(answered_note(&[], &s(&["Hi"])), "Answered by a person:\nHi");
        assert_eq!(answered_note(&[], &[]), "Answered by a person:\nCarry on");
    }
}
