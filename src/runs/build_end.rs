//! How a `build` or `fix` run ends (docs/specs/pieces.md section 3.2 and section 4): a pure mapping from the
//! implementer's answer, or the session's error, plus what was published, to the `(RunOutcome, result)` that
//! `run_done` carries. A `failed` result is `{}`: `run_done` adds the `why` from the outcome.
use anyhow::Error;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::backlog::runs::RunOutcome;
use crate::decisions::{Decision, split_asked};
use crate::implementer::publish::Published;
use crate::implementer::{
    CursorTimeKilled, ImplementerResult, Interrupted, LimitHit, MaxTurns, Question, SilenceKilled,
};

/// A pushed branch and the change opened for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub branch: String,
    pub change: Published,
}

/// Whether the work is pushed (and a change opened) before the run ends: for `pr_opened`, and for `questions`
/// when a committed slice is ahead of the base, so the next run can start as a `fix` from it.
pub fn needs_push(status: &str, commits_ahead: u64) -> bool {
    status == "pr_opened" || (status == "needs_clarification" && commits_ahead > 0)
}

fn failed(why: impl Into<String>) -> (RunOutcome, Value) {
    (RunOutcome::Failed(why.into()), json!({}))
}

/// The `branch`, `prUrl` and `prNumber` a result carries: the store's `run_done` for `pr_opened` takes exactly
/// these names (`pieceDone` in shared/backlogPieceOps.ts) and refuses the end with a 400 without them.
pub fn change_json(p: &Pushed) -> Value {
    json!({ "branch": p.branch, "prUrl": p.change.url, "prNumber": p.change.number })
}

/// A `questions` result's `what`: the store files it as what the piece waits for an answer on.
fn what_asked(questions: &[Question]) -> String {
    let what = questions
        .iter()
        .map(|q| q.question.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    // The store takes at most 4000 characters (`str(result, "what", 4000)`).
    let what: String = what.chars().take(4000).collect();
    if what.trim().is_empty() {
        "It has questions".to_string()
    } else {
        what
    }
}

/// The head of a person-steps reason, as the store's boards read it (`shared/personSteps.ts`).
const PERSON_STEPS_HEAD: &str = "Only a person can finish this. The code is all built; these steps are left:";

/// The reason that carries `steps`, numbered (`personStepsReason`).
pub fn person_steps_reason(steps: &[String]) -> String {
    let mut reason = PERSON_STEPS_HEAD.to_string();
    for (i, s) in steps.iter().enumerate() {
        reason.push_str(&format!(
            "\n{}. {}",
            i + 1,
            s.split_whitespace().collect::<Vec<_>>().join(" ")
        ));
    }
    reason
}

/// What the store takes for a `person_steps` answer: no such outcome exists for a build or fix, so (as the TypeScript
/// agent does, `waitForSteps` then `failed(needsHumanReason)`) the run ends `failed` with the steps as its why.
/// Any other end is left as it is.
pub fn person_steps_end(outcome: RunOutcome, result: Value) -> (RunOutcome, Value) {
    if outcome != RunOutcome::PersonSteps {
        return (outcome, result);
    }
    let steps: Vec<String> = result
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    failed(person_steps_reason(&steps))
}

/// A `questions` end: the questions (and the change, when a slice was pushed) with the `what` the store requires.
pub fn questions_end(pushed: Option<&Pushed>, questions: &[Question]) -> Value {
    let mut result = pushed.map(change_json).unwrap_or_else(|| json!({}));
    result["what"] = json!(what_asked(questions));
    result["questions"] = questions_json(questions);
    result
}

/// A result's `questions` array: each `{ header?, question, options }`.
pub fn questions_json(questions: &[Question]) -> Value {
    questions
        .iter()
        .map(|q| {
            let mut o = json!({ "question": q.question, "options": q.options });
            if !q.header.is_empty() {
                o["header"] = json!(q.header);
            }
            o
        })
        .collect()
}

/// A session that hit its engine's usage limit ends `limited` (pieces.md section 3.6), `until` being `now` plus
/// the time the engine said until the limit lifts. `None` for any other error.
pub fn limited_end(err: &Error, engine: &str, now: DateTime<Utc>) -> Option<(RunOutcome, Value)> {
    let hit = err.downcast_ref::<LimitHit>()?;
    let until = hit
        .resets_in
        .and_then(|d| chrono::Duration::from_std(d).ok())
        .and_then(|d| now.checked_add_signed(d));
    let engine = engine.to_string();
    Some((RunOutcome::Limited { engine, until }, json!({})))
}

/// The run's end from the implementer's answer. `decisions` are the answers already on the piece; `engine` is the
/// session's engine id.
pub fn end_from_answer(
    answer: Result<ImplementerResult, Error>,
    pushed: Option<&Pushed>,
    decisions: &[Decision],
    engine: &str,
    now: DateTime<Utc>,
) -> (RunOutcome, Value) {
    let r = match answer {
        Ok(r) => r,
        Err(e) if limited_end(&e, engine, now).is_some() => {
            return limited_end(&e, engine, now).expect("a LimitHit");
        }
        Err(e) if e.downcast_ref::<Interrupted>().is_some() => return (RunOutcome::Interrupted, json!({})),
        Err(e) if e.downcast_ref::<SilenceKilled>().is_some() => {
            return failed(format!("went quiet and was killed: {e:#}"));
        }
        Err(e) if e.downcast_ref::<CursorTimeKilled>().is_some() => {
            return failed(format!("ran past its time limit and was killed: {e:#}"));
        }
        Err(e) if e.downcast_ref::<MaxTurns>().is_some() => return failed(format!("ran out of turns: {e:#}")),
        Err(e) => return failed(format!("The session ended without answering: {e:#}")),
    };
    match r.status.as_str() {
        "pr_opened" => match pushed {
            Some(p) => (RunOutcome::PrOpened, change_json(p)),
            None => failed("The implementer finished but nothing was pushed"),
        },
        "needs_clarification" => {
            let asked: Vec<String> = r.questions.iter().map(|q| q.question.clone()).collect();
            let (answered, _) = split_asked(&asked, decisions);
            if !answered.is_empty() {
                return failed(format!(
                    "The implementer asked again what a person already answered: {}",
                    answered
                        .iter()
                        .map(|d| d.question.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            (RunOutcome::Questions, questions_end(pushed, &r.questions))
        }
        "person_steps" if r.person_steps.is_empty() => failed(format!("Implementer gave up: {}", r.summary)),
        "person_steps" => (RunOutcome::PersonSteps, json!({ "steps": r.person_steps })),
        "nothing_left" => (RunOutcome::NothingLeft, json!({})),
        "failed" => failed(r.summary),
        "waits_on" => {
            let needs = if r.waits_on.is_empty() {
                "another item".to_string()
            } else {
                r.waits_on.join(", ")
            };
            failed(format!("Needs {needs} built first: {}", r.summary))
        }
        other => failed(format!("The session answered with {other}: {}", r.summary)),
    }
}

#[cfg(test)]
#[path = "build_end_tests.rs"]
mod tests;
