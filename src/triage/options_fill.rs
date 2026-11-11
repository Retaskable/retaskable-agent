//! Options for a question that has none (docs/specs/questions-and-triage.md, `optionsFill.ts`): an item or draft in
//! `needs_clarification` whose question has fewer than 2 options gets one read-only session, run as the triager is
//! (middle tier, `budget.reviewerUsd`), that returns 2 to 4 options per question, the recommended first. After
//! [`MAX_FILLS`] failed sessions the fixed [`FALLBACK_OPTIONS`] stand in, so nothing waits for ever. This is the pure
//! part: what to ask, how to read the answer and what to do with it ([`next`]); a caller runs the session and keeps
//! the record's marks and the retry time.
use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec};
use crate::implementer::model::Question;
use crate::prompt::render_prompt;

pub const TEMPLATE: &str = include_str!("../../prompts/options-fill.md");
/// Failed sessions before the fixed choices are saved.
pub const MAX_FILLS: u32 = 2;
/// How long a failed session waits before the next one.
pub const RETRY: Duration = Duration::from_secs(10 * 60);
/// The fixed choices; the label itself is recorded as the answer.
pub const FALLBACK_OPTIONS: [&str; 2] = ["Go with what Claude would pick", "Skip this"];

fn lacks(q: &Question) -> bool {
    q.options.len() < 2
}

/// Whether any of the questions has fewer than 2 options.
pub fn needs_options(questions: &[Question]) -> bool {
    questions.iter().any(lacks)
}

/// The questions with `options[i]` set on each one that lacks them; the others keep theirs.
pub fn filled(questions: &[Question], options: &[Vec<String>]) -> Vec<Question> {
    questions
        .iter()
        .zip(options)
        .map(|(q, o)| {
            let mut q = q.clone();
            if lacks(&q) {
                q.options = o.clone();
            }
            q
        })
        .collect()
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "options": {
                        "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 4,
                        "description": "The 2 to 4 answers you'd suggest, the one you recommend first, the safe, sensible choice."
                    } },
                    "required": ["options"]
                }
            }
        },
        "required": ["questions"]
    })
}

#[derive(Deserialize)]
struct Answer {
    questions: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    options: Vec<String>,
}

/// The read-only session for one record: `about` is what the item or plan request is, then its questions numbered.
pub fn session_spec(about: &str, questions: &[Question], cfg: &EphemeralSessionConfig<'_>) -> Result<SessionSpec> {
    let list = questions
        .iter()
        .enumerate()
        .map(|(i, q)| format!("{}. {}", i + 1, q.question))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = render_prompt(TEMPLATE, &[("about", about.to_string()), ("questions", list)])?;
    Ok(cfg.spec(prompt, schema()))
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// One list per question, each of 2 to 4 where the question lacked options.
    Options(Vec<Vec<String>>),
    /// No result, or one that doesn't fit.
    Failed,
    /// The usage limit: counts nothing.
    Limit,
}

/// Reads a finished session against the questions that were asked.
pub fn parse(r: &SessionResult, asked: &[Question]) -> Ended {
    let got =
        r.ok.then(|| r.structured.clone())
            .flatten()
            .and_then(|v| serde_json::from_value::<Answer>(v).ok())
            .map(|a| a.questions.into_iter().map(|e| e.options).collect::<Vec<_>>());
    match got {
        Some(got)
            if got.len() == asked.len()
                && got
                    .iter()
                    .zip(asked)
                    .all(|(o, q)| !lacks(q) || (2..=4).contains(&o.len())) =>
        {
            Ended::Options(got)
        }
        _ if !r.ok && r.limit_hit => Ended::Limit,
        _ => Ended::Failed,
    }
}

/// Runs the session through `run` (the caller's [`crate::engines::run_session`]) and reads it. A session that can't
/// even start is a failed one.
pub async fn fill<F, Fut>(about: &str, questions: &[Question], cfg: &EphemeralSessionConfig<'_>, run: F) -> Ended
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let Ok(spec) = session_spec(about, questions, cfg) else {
        return Ended::Failed;
    };
    match run(spec).await {
        Ok(r) => parse(&r, questions),
        Err(_) => Ended::Failed,
    }
}

/// What to do with an ended session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// The record was answered, or its questions all have options, meanwhile: leave it alone.
    Leave,
    /// A usage limit: nothing counted; the next try waits for the limit to lift.
    WaitForLimit,
    /// A failed session short of [`MAX_FILLS`]: save `tries` and try again after [`RETRY`].
    Retry { tries: u32 },
    /// Save `questions` and `tries`; `fixed` when these are the fixed choices.
    Save {
        questions: Vec<Question>,
        tries: u32,
        fixed: bool,
    },
}

/// `current` is the record's questions now (`None` when it left `needs_clarification` meanwhile), `tries` its
/// `optionsTries`.
pub fn next(ended: Ended, current: Option<&[Question]>, tries: u32) -> Next {
    let Some(current) = current.filter(|q| needs_options(q)) else {
        return Next::Leave;
    };
    let good = match ended {
        Ended::Limit => return Next::WaitForLimit,
        Ended::Options(o) if o.len() == current.len() => Some(o),
        _ => None,
    };
    let tries = tries + u32::from(good.is_none());
    match good {
        Some(options) => Next::Save {
            questions: filled(current, &options),
            tries,
            fixed: false,
        },
        None if tries < MAX_FILLS => Next::Retry { tries },
        None => {
            let fixed = vec![FALLBACK_OPTIONS.map(String::from).to_vec(); current.len()];
            Next::Save {
                questions: filled(current, &fixed),
                tries,
                fixed: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::engines::engine::test_result;

    fn q(text: &str, options: &[&str]) -> Question {
        Question {
            header: String::new(),
            question: text.to_string(),
            options: options.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn cfg() -> EphemeralSessionConfig<'static> {
        EphemeralSessionConfig {
            cwd: Path::new("."),
            budget_usd: 1.0,
            model: "sonnet".into(),
            local: None,
            engine: None,
            cursor: None,
            sandbox: false,
            shell: true,
        }
    }

    fn result(v: Value) -> SessionResult {
        test_result(true, "success", Some(v), None)
    }

    #[test]
    fn only_questions_that_lacked_options_get_them() {
        let qs = [q("A?", &[]), q("B?", &["x", "y"]), q("C?", &["only"])];
        assert!(needs_options(&qs) && !needs_options(&qs[1..2]));
        let out = filled(
            &qs,
            &[
                vec!["1".into(), "2".into()],
                vec!["no".into(), "no".into()],
                vec!["3".into(), "4".into()],
            ],
        );
        assert_eq!(out[0].options, ["1", "2"]);
        assert_eq!(out[1].options, ["x", "y"]);
        assert_eq!(out[2].options, ["3", "4"]);
    }

    #[test]
    fn the_prompt_numbers_the_questions() {
        let s = session_spec("Item x", &[q("A?", &[]), q("B?", &[])], &cfg()).unwrap();
        assert!(s.prompt.contains("Item x") && s.prompt.contains("1. A?\n2. B?"));
        assert!(s.readonly);
    }

    #[tokio::test]
    async fn a_good_answer_is_read_and_a_misfit_or_failure_is_failed() {
        let asked = [q("A?", &[]), q("B?", &["x", "y"])];
        let good = json!({ "questions": [{ "options": ["a", "b"] }, { "options": ["z"] }] });
        let ended = fill("about", &asked, &cfg(), |_| async { Ok(result(good)) }).await;
        assert_eq!(
            ended,
            Ended::Options(vec![vec!["a".into(), "b".into()], vec!["z".into()]])
        );
        for bad in [
            json!({ "questions": [{ "options": ["a"] }, { "options": [] }] }),
            json!({ "questions": [{ "options": ["a", "b"] }] }),
            json!({ "nope": 1 }),
        ] {
            assert_eq!(parse(&result(bad), &asked), Ended::Failed);
        }
        let err = fill("about", &asked, &cfg(), |_| async { anyhow::bail!("x") }).await;
        assert_eq!(err, Ended::Failed);
    }

    #[test]
    fn a_limit_is_not_a_failed_session() {
        let mut r = test_result(false, "error", None, None);
        r.limit_hit = true;
        assert_eq!(parse(&r, &[q("A?", &[])]), Ended::Limit);
        assert_eq!(next(Ended::Limit, Some(&[q("A?", &[])]), 0), Next::WaitForLimit);
    }

    #[test]
    fn two_failures_save_the_fixed_choices() {
        let cur = [q("A?", &[]), q("B?", &["x", "y"])];
        assert_eq!(next(Ended::Failed, Some(&cur), 0), Next::Retry { tries: 1 });
        let Next::Save {
            questions,
            tries,
            fixed,
        } = next(Ended::Failed, Some(&cur), 1)
        else {
            panic!()
        };
        assert!(fixed && tries == 2);
        assert_eq!(questions[0].options, FALLBACK_OPTIONS);
        assert_eq!(questions[1].options, ["x", "y"]);
    }

    #[test]
    fn a_good_answer_saves_without_counting_and_an_answered_record_is_left() {
        let cur = [q("A?", &[])];
        let n = next(Ended::Options(vec![vec!["a".into(), "b".into()]]), Some(&cur), 1);
        assert!(matches!(
            n,
            Next::Save {
                tries: 1,
                fixed: false,
                ..
            }
        ));
        assert_eq!(next(Ended::Failed, None, 0), Next::Leave);
        assert_eq!(next(Ended::Failed, Some(&[q("A?", &["a", "b"])]), 0), Next::Leave);
    }
}
