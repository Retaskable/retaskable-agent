//! Running an ideas session and picking the round (`generate`/`write`, agent/src/ideas.ts): the
//! read-only, ephemeral `writer` session, run through the caller's
//! [`crate::engines::runner::run_session`], folded into the queue with [`super::model::refilled`].
use std::future::Future;

use anyhow::Result;
use serde_json::Value;

use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};

use super::model::{Idea, IdeasResult, Queue, ideas_result_schema, refilled};
use super::profiles::{Profile, build_prompt};

/// What an ideas session runs with, besides what prompted it and the room left in the queue.
pub struct Writer<'a> {
    /// `prompts/ideas.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    pub github_repo: &'a str,
    /// `Role::Writer`'s engine/model/budget/local/cursor/sandbox bundle; `session.cwd` is a
    /// checkout of the project, where the session may read its specs.
    pub session: EphemeralSessionConfig<'a>,
}

/// The read-only, ephemeral `writer` session for a round.
pub fn session_spec(
    w: &Writer<'_>,
    after: &str,
    room: usize,
    profiles: &[Profile],
    context: &Value,
) -> Result<SessionSpec> {
    let prompt = build_prompt(
        w.template,
        w.project_type,
        w.github_repo,
        after,
        room,
        profiles,
        context,
    )?;
    Ok(w.session.spec(prompt, ideas_result_schema()))
}

/// A round's own outcome: the queue's new ideas (unchanged from `was` when it fails), the model that
/// ran (empty when none did), its cost, and why it fell back when it did.
#[derive(Debug, Clone)]
pub struct Round {
    pub ideas: Vec<Idea>,
    pub model: String,
    pub cost_usd: f64,
    pub error: Option<String>,
}

/// `generate` and `write` (agent/src/ideas.ts): asks for however much room is left in the queue (at
/// least one), runs the session through `run` (the caller's
/// [`crate::engines::runner::run_session`]), and folds its answer into the queue with [`refilled`].
/// Leaves the queue waiting, as it was, when the session fails or doesn't answer `ok`.
#[allow(clippy::too_many_arguments)]
pub async fn run_round<F, Fut>(
    was: &Queue,
    w: &Writer<'_>,
    after: &str,
    profiles: &[Profile],
    context: &Value,
    queue: usize,
    max_waited: u32,
    stamp: &str,
    run: F,
) -> Round
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let done = |ideas: Vec<Idea>, model: String, cost_usd: f64, error: Option<String>| Round {
        ideas,
        model,
        cost_usd,
        error: error.map(|e| format!("Claude {e}.")),
    };
    let room = queue.saturating_sub(was.ideas.len()).max(1);
    let spec = match session_spec(w, after, room, profiles, context) {
        Ok(spec) => spec,
        Err(e) => {
            return done(
                was.ideas.clone(),
                String::new(),
                0.0,
                Some(format!("couldn't think of ideas ({e:#})")),
            );
        }
    };
    let model = spec.model.clone();
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => {
            return done(
                was.ideas.clone(),
                model,
                0.0,
                Some(format!("couldn't think of ideas ({e:#})")),
            );
        }
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return done(
            was.ideas.clone(),
            model,
            cost_usd,
            Some(describe_failure(&r, w.session.budget_usd)),
        );
    }
    let res = r.structured.and_then(|v| serde_json::from_value::<IdeasResult>(v).ok());
    match res {
        Some(res) if res.status == "ok" => {
            let profile_ids: Vec<String> = profiles.iter().map(|p| p.id.clone()).collect();
            let ideas = refilled(was, &res, &profile_ids, stamp, queue, max_waited);
            done(ideas, model, cost_usd, None)
        }
        other => {
            let why = other.and_then(|r| r.error).filter(|e| !e.is_empty());
            let error = format!(
                "couldn't think of ideas{}",
                why.map(|e| format!(": {e}")).unwrap_or_default()
            );
            done(was.ideas.clone(), model, cost_usd, Some(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::engines::engine::test_result as result;
    use crate::ideas::model::{IDEAS_MAX_WAITS_DEFAULT, QUEUE};
    use crate::ideas::test_support::waiting_idea;

    const TEMPLATE: &str = "{{projectKind}} {{githubRepo}} {{after}} {{room}} {{profiles}} {{context}}";

    fn writer(model: &str, cwd: &'static Path) -> Writer<'static> {
        Writer {
            template: TEMPLATE,
            project_type: ProjectType::Web,
            github_repo: "x/y",
            session: EphemeralSessionConfig {
                cwd,
                budget_usd: 0.5,
                model: model.to_string(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
                shell: true,
            },
        }
    }

    fn one_waiting() -> Queue {
        Queue {
            ideas: vec![waiting_idea("A", 0)],
            ..Queue::default()
        }
    }

    /// `run_round` with the args every test below shares, bar `was` and `run`.
    async fn run_chores<F, Fut>(was: &Queue, cwd: &'static Path, run: F) -> Round
    where
        F: FnOnce(SessionSpec) -> Fut,
        Fut: Future<Output = Result<SessionResult>>,
    {
        run_round(
            was,
            &writer("haiku", cwd),
            "Chores",
            &[],
            &json!({}),
            QUEUE,
            IDEAS_MAX_WAITS_DEFAULT,
            "t",
            run,
        )
        .await
    }

    #[tokio::test]
    async fn run_round_asks_for_the_room_left_in_the_queue() {
        let was = Queue {
            ideas: vec![waiting_idea("A", 0), waiting_idea("B", 0)],
            ..Queue::default()
        };
        let cwd = Path::new("repo");
        let round = run_chores(&was, cwd, |spec| async move {
            assert!(
                spec.prompt.contains(" 3 "),
                "asks for the 3 slots still free: {}",
                spec.prompt
            );
            Ok(result(
                true,
                "success",
                Some(json!({ "status": "ok", "ideas": [] })),
                None,
            ))
        })
        .await;
        assert!(round.error.is_none());
    }

    #[tokio::test]
    async fn run_round_asks_for_at_least_one_even_when_the_queue_is_already_full() {
        let was = Queue {
            ideas: (0..QUEUE).map(|n| waiting_idea(&format!("I{n}"), 0)).collect(),
            ..Queue::default()
        };
        let cwd = Path::new("repo");
        run_chores(&was, cwd, |spec| async move {
            assert!(spec.prompt.contains(" 1 "), "asks for at least 1: {}", spec.prompt);
            Ok(result(
                true,
                "success",
                Some(json!({ "status": "ok", "ideas": [] })),
                None,
            ))
        })
        .await;
    }

    #[tokio::test]
    async fn run_rounds_answer_refills_the_queue() {
        let was = Queue::default();
        let profiles = vec![Profile {
            id: "shopper".to_string(),
            name: "Shopper".to_string(),
            who: "Buys things".to_string(),
            part: "shop".to_string(),
        }];
        let cwd = Path::new("repo");
        let answer = json!({
            "status": "ok",
            "ideas": [{
                "title": "Steps you can tick off",
                "experience": "Tap it.",
                "why": "Clearer.",
                "whyNow": "Checkout shipped.",
                "for": ["shopper"],
                "feature": "",
                "newFeature": "Checklist",
            }],
        });
        let round = run_round(
            &was,
            &writer("haiku", cwd),
            "Checkout",
            &profiles,
            &json!({}),
            QUEUE,
            IDEAS_MAX_WAITS_DEFAULT,
            "t",
            |_| async move { Ok(result(true, "success", Some(answer), None)) },
        )
        .await;
        assert_eq!(round.ideas.len(), 1);
        assert_eq!(round.ideas[0].title, "Steps you can tick off");
        assert_eq!(round.ideas[0].new_feature, Some("Checklist".to_string()));
        assert_eq!(round.model, "haiku");
        assert_eq!(round.cost_usd, 0.12);
        assert!(round.error.is_none());
    }

    #[tokio::test]
    async fn a_failed_session_leaves_the_queue_waiting_and_says_why() {
        let was = one_waiting();
        let cwd = Path::new("repo");
        let round = run_chores(&was, cwd, |_| async {
            Ok(result(false, "error_max_budget_usd", None, None))
        })
        .await;
        assert_eq!(round.ideas, was.ideas);
        assert_eq!(
            round.error.as_deref(),
            Some("Claude hit its $0.5 budget (spent $0.12).")
        );
    }

    #[tokio::test]
    async fn a_failed_answer_or_a_session_that_wont_start_leaves_the_queue_waiting() {
        let was = one_waiting();
        let cwd = Path::new("repo");
        let failed = json!({ "status": "failed", "error": "no ideas come to mind" });
        let round = run_chores(
            &was,
            cwd,
            |_| async move { Ok(result(true, "success", Some(failed), None)) },
        )
        .await;
        assert_eq!(round.ideas, was.ideas);
        assert_eq!(
            round.error.as_deref(),
            Some("Claude couldn't think of ideas: no ideas come to mind.")
        );

        let round = run_chores(&was, cwd, |_| async { anyhow::bail!("no claude") }).await;
        assert_eq!(round.ideas, was.ideas);
        assert_eq!(
            round.error.as_deref(),
            Some("Claude couldn't think of ideas (no claude).")
        );
        assert_eq!(round.cost_usd, 0.0);
    }
}
