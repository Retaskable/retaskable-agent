//! What a plan's drafter asked and what a person answered (simple-flow/03 section 3.7, TS `splitAsked` in `drafts.ts`). The
//! drafter that asks a question a person already answered gets the answer back, not the person: a drafter's questions are
//! matched (same text, ignoring case and spacing) against the answers kept for the plan. All of them answered: the same
//! session is resumed with the answers, up to the store's `maxReasks` times, after which the plan fails with the reason. Some
//! still open: only those go to the person. A new answer resets the count (the count is the run's own, so it starts at 0).
//!
//! The store keeps only the latest answer on a draft and clears it once taken, so what was asked and what was answered is kept
//! here, in a file of the project's own, by draft id: the questions put to a person, and once the answers come (in the same
//! order) each pair. Lost with the file, a repeated question simply goes to a person again.
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::{BacklogError, Ctx, DraftReport};
use crate::decisions::{Decision, split_asked};
use crate::drafts::failure::{NO_REASON, error_reason, reason_or};
use crate::drafts::session::{Outcome, sort_fixes};
use crate::drafts::{DrafterResult, DrafterStatus};
use crate::implementer::Interrupted;
use crate::owed_file::{KEPT_SECS, OwedFile, now_secs};

/// The file in the project's folder.
pub(super) const FILE: &str = "draft_questions.json";

/// Kept for a week: a plan waiting that long for its answer is as good as new.
const KEPT_FOR: i64 = 7 * KEPT_SECS;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(in crate::orchestrator::driver) struct Asked {
    /// The questions put to a person, waiting for their answers.
    #[serde(default)]
    asked: Vec<String>,
    /// Each question a person has answered, with the answer.
    #[serde(default)]
    decided: Vec<(String, String)>,
    /// Epoch seconds, of the last change.
    #[serde(default)]
    at: i64,
}

pub(in crate::orchestrator::driver) type Kept = OwedFile<Asked>;

pub(in crate::orchestrator::driver) fn open(dir: &std::path::Path) -> Kept {
    OwedFile::open(dir.join(FILE))
}

/// A person's answers arrived for the questions last put to them: each pair is now decided.
pub(super) fn answered(kept: &Kept, id: &str, answers: &[String]) {
    let mut now = kept.get(id).unwrap_or_default();
    if now.asked.is_empty() || now.asked.len() != answers.len() {
        return;
    }
    let asked = std::mem::take(&mut now.asked);
    now.decided.extend(asked.into_iter().zip(answers.iter().cloned()));
    now.at = now_secs();
    kept.put(id, now);
}

fn decisions(kept: &Kept, id: &str) -> Vec<Decision> {
    (kept.get(id).unwrap_or_default().decided.into_iter())
        .map(|(question, answer)| Decision {
            question,
            answer,
            at: String::new(),
        })
        .collect()
}

/// Puts `asked` as the questions waiting for a person, dropping what has been kept too long for any plan.
fn waiting(kept: &Kept, id: &str, asked: Vec<String>) {
    let mut now = kept.get(id).unwrap_or_default();
    now.asked = asked;
    now.at = now_secs();
    kept.put(id, now);
    let cutoff = now_secs() - KEPT_FOR;
    for (other, entry) in kept.all() {
        if entry.at < cutoff {
            kept.remove(&other);
        }
    }
}

/// The follow-up that sends the answers back.
fn answers_back(answered: &[Decision]) -> String {
    let pairs: Vec<String> = (answered.iter())
        .map(|d| format!("Q: {}\nA: {}", d.question, d.answer))
        .collect();
    format!(
        "You asked questions a person has already answered. Their answers:\n\n{}\n\nAct on them now, then return the result. Don't ask them again.",
        pairs.join("\n\n")
    )
}

/// What the drafter's result comes to once the questions it asks are matched against the answers.
pub(super) enum Drafted {
    /// Carry on with this result: no question, or only open ones (the result's `questions` are then those).
    Done(Box<DrafterResult>),
    /// The plan ends here, as this report says.
    Ended(Result<(), BacklogError>),
}

/// Matches the questions `result` asks against the answers kept for draft `id`. All answered: `rerun(session, follow_up)`
/// resumes the session with them, up to `maxReasks` times. Some open: `result.questions` is cut to those. What is put to a person
/// is kept, to pair with their answers.
pub(super) async fn settle(
    ctx: &Ctx,
    id: &str,
    mut result: DrafterResult,
    session: &str,
    rerun: impl Fn(String, String) -> BoxFuture<'static, anyhow::Result<DrafterResult>>,
) -> Drafted {
    let mut reasks = 0u32;
    let mut session = session.to_string();
    loop {
        let sorted = sort_fixes(&result);
        // A piece the drafter couldn't class is asked as Fix / Tweak / Feature, and never matched.
        if let Outcome::Ask(qs) = &sorted.outcome {
            waiting(&ctx.asked, id, qs.iter().map(|q| q.question.clone()).collect());
            return Drafted::Done(Box::new(result));
        }
        if result.status != DrafterStatus::NeedsClarification {
            return Drafted::Done(Box::new(result));
        }
        let asked: Vec<String> = result.questions.iter().map(|q| q.question.clone()).collect();
        let (answered, open) = split_asked(&asked, &decisions(&ctx.asked, id));
        if answered.is_empty() {
            waiting(&ctx.asked, id, asked);
            return Drafted::Done(Box::new(result));
        }
        if !open.is_empty() {
            // Only what no one has answered goes to the person.
            result.questions.retain(|q| open.contains(&q.question));
            waiting(&ctx.asked, id, open);
            return Drafted::Done(Box::new(result));
        }
        reasks += 1;
        // The cap is the store's `maxReasks`; unknown, the plan goes to a person at once.
        let max = ctx.project.tuning.store.map_or(0, |t| t.max_reasks);
        if reasks > max {
            let names: Vec<&str> = answered.iter().map(|a| a.question.as_str()).collect();
            let why = format!(
                "The drafter keeps asking questions a person already answered ({}): {}",
                names.join("; "),
                result.summary
            );
            let report = DraftReport {
                summary: Some(reason_or(&why, NO_REASON)),
                ..Default::default()
            };
            return Drafted::Ended(ctx.needs_person(id, "failed", report).await);
        }
        tracing::info!(
            "{id}: asked again what a person already answered ({}); sent the answers back ({reasks}/{max})",
            answered.len()
        );
        match rerun(session.clone(), answers_back(&answered)).await {
            Ok(next) => {
                if let Some(chat) = next.session_id.clone() {
                    session = chat;
                }
                result = next;
            }
            // Cut by a restart or Quit: left as it is, to be claimed and resumed by its session id.
            Err(e) if e.downcast_ref::<Interrupted>().is_some() => return Drafted::Ended(Ok(())),
            Err(e) => {
                let report = DraftReport {
                    summary: Some(error_reason(&format!("{e:#}"), "error")),
                    ..Default::default()
                };
                return Drafted::Ended(ctx.failed(id, report).await);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept() -> (tempfile::TempDir, Kept) {
        let dir = tempfile::tempdir().unwrap();
        let k = open(dir.path());
        (dir, k)
    }

    #[test]
    fn answers_pair_with_the_questions_last_put_to_the_person_and_are_kept() {
        let (dir, k) = kept();
        waiting(&k, "d1", vec!["Squash?".into(), "Which branch?".into()]);
        answered(&k, "d1", &["Yes".into(), "main".into()]);
        let again = open(dir.path());
        let d = decisions(&again, "d1");
        assert_eq!(
            d.iter()
                .map(|d| (d.question.as_str(), d.answer.as_str()))
                .collect::<Vec<_>>(),
            [("Squash?", "Yes"), ("Which branch?", "main")]
        );
        // The same answers arriving again pair with nothing.
        answered(&k, "d1", &["Yes".into(), "main".into()]);
        assert_eq!(decisions(&k, "d1").len(), 2);
    }

    #[test]
    fn answers_that_do_not_match_the_questions_in_number_are_not_paired() {
        let (_dir, k) = kept();
        waiting(&k, "d1", vec!["Squash?".into()]);
        answered(&k, "d1", &["a".into(), "b".into()]);
        assert!(decisions(&k, "d1").is_empty());
    }

    #[test]
    fn a_question_asked_again_matches_its_answer_and_the_follow_up_carries_it() {
        let (_dir, k) = kept();
        waiting(&k, "d1", vec!["Squash or merge?".into()]);
        answered(&k, "d1", &["Squash".into()]);
        let (done, open) = split_asked(&["squash  or MERGE?".into(), "New?".into()], &decisions(&k, "d1"));
        assert_eq!((done.len(), open), (1, vec!["New?".to_string()]));
        let text = answers_back(&done);
        assert!(text.contains("Q: Squash or merge?\nA: Squash") && text.contains("Don't ask them again."));
    }
}
