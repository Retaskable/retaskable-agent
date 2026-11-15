//! A `triage` task (docs/specs/questions-and-triage.md, `triageTask.ts`): the store sends what it knows of an item parked
//! for help, and this runs the read-only session and answers the structured result. It decides nothing: whether to
//! look, the second look, the limits, the retries and what the item becomes are the store's. `look: "check"` is the
//! top tier's check of a clear suggestion; anything else is the first look. A Retaskable bug the session names is
//! answered in [`Outcome::Answered`] for the caller to send scrubbed ([`super::bug_report`]), since this computer holds
//! the consent and the known bugs.
use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::ProjectType;
use crate::engines::engine::resets_in;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec};

use super::second_look::{SecondLook, SecondLookContext, check};
use super::{KnownBug, RetaskableBug, TriageButton, TriageContext, Triager, decide};

/// What a task's args name of the item (`runTriage`'s `job.args`).
fn text(a: &Value, key: &str) -> Option<String> {
    a.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Whether the task asks for the check of a suggestion rather than the first look.
pub fn is_check(args: &Value) -> bool {
    args.get("look").and_then(Value::as_str) == Some("check")
}

/// The item's facts as the triage prompt reads them, from the task's args.
pub fn context_of(args: &Value, known_bugs: Vec<KnownBug>) -> TriageContext {
    let timeline = args
        .get("timeline")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(|l| {
                    let body = l.get("text")?.as_str()?;
                    Some(
                        format!("{} {body}", l.get("at").and_then(Value::as_str).unwrap_or(""))
                            .trim()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let branch = text(args, "branch");
    TriageContext {
        reason: text(args, "reason").unwrap_or_else(|| "(no reason recorded)".to_string()),
        failed_in: text(args, "failedIn").unwrap_or_else(|| "unknown".to_string()),
        timeline,
        last_check_result: args
            .get("checks")
            .filter(|c| c.as_array().is_some_and(|c| !c.is_empty()))
            .map(Value::to_string),
        pr_url: text(args, "prUrl"),
        has_pushed_branch: branch.is_some(),
        has_session_to_continue: args
            .get("hasSessionToContinue")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        branch,
        automatic_decisions: args.get("triages").and_then(Value::as_u64).unwrap_or(0) as u32,
        transcripts_dir: None,
        known_bugs,
    }
}

/// What the task answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `{ triage }` or `{ check }`: the session's structured result as it came, and the Retaskable bug it named.
    Answered { result: Value, bug: Option<RetaskableBug> },
    /// `ok: false` with a note; `no_retry` for a budget stop, and `limit` for a usage limit (when it lifts, if known).
    Failed {
        note: String,
        no_retry: bool,
        limit: Option<Option<Duration>>,
    },
}

/// What the task's sessions run with: `first` for the look, `top` for the check (the caller picks the tier).
pub struct Task<'a> {
    pub triage_template: &'a str,
    pub check_template: &'a str,
    pub project_type: ProjectType,
    pub github_repo: &'a str,
    pub max_automatic_decisions: u32,
    pub session: EphemeralSessionConfig<'a>,
}

/// What the session said about how it ended, kept beside the result.
#[derive(Default)]
struct Ended {
    structured: Option<Value>,
    subtype: String,
    limit: Option<Option<Duration>>,
}

fn failed(note: Option<String>, e: &Ended, what: &str) -> Outcome {
    Outcome::Failed {
        note: format!("it {}", note.unwrap_or_else(|| format!("couldn't {what}"))),
        no_retry: e.subtype == "error_max_budget_usd",
        limit: e.limit,
    }
}

/// Runs the task's session through `run` (the caller's [`crate::engines::run_session`]). Never decides anything.
pub async fn run<F, Fut>(args: &Value, ctx: TriageContext, t: &Task<'_>, run: F) -> Outcome
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let mut ended = Ended::default();
    let seen = &mut ended;
    let tracked = |spec| async {
        let r = run(spec).await?;
        seen.structured = r.structured.clone();
        seen.subtype = r.subtype.clone();
        seen.limit = r.limit_hit.then(|| resets_in(r.usage.as_ref()));
        Ok(r)
    };
    if is_check(args) {
        let first = args.get("first").cloned().unwrap_or(Value::Null);
        let reason = text(&first, "reason").unwrap_or_default();
        let instructions = text(&first, "instructions").unwrap_or_default();
        let suggest = match text(&first, "suggest").as_deref() {
            Some("continue") => TriageButton::Continue,
            Some("skip") => TriageButton::Skip,
            _ => TriageButton::StartOver,
        };
        let look = SecondLook {
            template: t.check_template,
            project_type: t.project_type,
            github_repo: t.github_repo,
            session: t.session.clone(),
            max_automatic_decisions: t.max_automatic_decisions,
        };
        let cx = SecondLookContext {
            triage: &ctx,
            triager_reason: &reason,
            triager_suggest: suggest,
            triager_instructions: &instructions,
        };
        let checked = check(&look, &cx, tracked).await;
        return match checked.result {
            Some(_) => Outcome::Answered {
                result: json!({ "check": ended.structured.unwrap_or(Value::Null) }),
                bug: None,
            },
            None => failed(checked.note, &ended, "check it"),
        };
    }
    let triager = Triager {
        template: t.triage_template,
        project_type: t.project_type,
        github_repo: t.github_repo,
        session: t.session.clone(),
        max_automatic_decisions: t.max_automatic_decisions,
    };
    let decided = decide(&triager, &ctx, tracked).await;
    match decided.result {
        Some(r) => Outcome::Answered {
            result: json!({ "triage": ended.structured.unwrap_or(Value::Null) }),
            bug: r.retaskable_bug,
        },
        None => failed(decided.note, &ended, "look into it"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine::{test_config, test_result};

    fn task() -> Task<'static> {
        Task {
            triage_template: "{{projectKind}} {{githubRepo}} {{context}}",
            check_template: "{{projectKind}} {{githubRepo}} {{context}}",
            project_type: ProjectType::Web,
            github_repo: "x/y",
            max_automatic_decisions: 2,
            session: test_config(),
        }
    }

    fn args() -> Value {
        json!({
            "id": "a", "reason": "CI red", "failedIn": "reviewing", "branch": "b", "prUrl": "http://pr/1", "triages": 1,
            "timeline": [{ "at": "10:00", "text": "failed" }, { "nope": 1 }], "checks": []
        })
    }

    #[test]
    fn the_context_comes_from_the_args() {
        let c = context_of(&args(), vec![]);
        assert_eq!(
            (c.reason.as_str(), c.failed_in.as_str(), c.automatic_decisions),
            ("CI red", "reviewing", 1)
        );
        assert_eq!(c.timeline, ["10:00 failed"]);
        assert!(c.has_pushed_branch && c.last_check_result.is_none());
        let bare = context_of(&json!({}), vec![]);
        assert_eq!(
            (bare.reason.as_str(), bare.failed_in.as_str()),
            ("(no reason recorded)", "unknown")
        );
        assert!(!bare.has_pushed_branch);
    }

    #[tokio::test]
    async fn a_look_answers_its_triage_and_hands_back_the_retaskable_bug() {
        let v = json!({ "decision": "human", "reason": "r", "retaskableBug": { "title": "t", "details": "d" } });
        let out = run(&args(), context_of(&args(), vec![]), &task(), |_| async {
            Ok(test_result(true, "success", Some(v), None))
        })
        .await;
        let Outcome::Answered { result, bug } = out else {
            panic!("{out:?}")
        };
        assert_eq!(result["triage"]["decision"], "human");
        assert_eq!(bug.unwrap().title, "t");
    }

    #[tokio::test]
    async fn a_check_answers_its_check() {
        let mut a = args();
        a["look"] = json!("check");
        a["first"] = json!({ "reason": "r", "suggest": "skip" });
        let out = run(&a, context_of(&a, vec![]), &task(), |spec| async move {
            assert!(spec.prompt.contains("skip"));
            Ok(test_result(
                true,
                "success",
                Some(json!({ "agree": true, "reason": "ok" })),
                None,
            ))
        })
        .await;
        let Outcome::Answered { result, bug } = out else {
            panic!("{out:?}")
        };
        assert_eq!(result["check"]["agree"], true);
        assert!(bug.is_none());
    }

    #[tokio::test]
    async fn a_failure_says_why_a_budget_stop_is_no_retry_and_a_limit_names_itself() {
        let a = args();
        let out = run(&a, context_of(&a, vec![]), &task(), |_| async {
            Ok(test_result(false, "error_max_budget_usd", None, None))
        })
        .await;
        let Outcome::Failed { note, no_retry, limit } = out else {
            panic!()
        };
        assert!(note.starts_with("it hit its $1 budget") && no_retry && limit.is_none());
        let out = run(&a, context_of(&a, vec![]), &task(), |_| async {
            let mut r = test_result(false, "error", None, None);
            r.limit_hit = true;
            Ok(r)
        })
        .await;
        let Outcome::Failed { note, no_retry, limit } = out else {
            panic!()
        };
        assert!(note.contains("usage limit") && !no_retry && limit.is_some());
        let out = run(&a, context_of(&a, vec![]), &task(), |_| async {
            Ok(test_result(true, "success", Some(json!({ "decision": "nope" })), None))
        })
        .await;
        assert!(matches!(
            out,
            Outcome::Failed {
                no_retry: false,
                limit: None,
                ..
            }
        ));
    }
}
