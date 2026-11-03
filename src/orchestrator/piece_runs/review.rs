//! Review of a build or fix run's pushed change (docs/specs/pieces.md section 3.2; stages 2 and 3 of
//! agent/06.3-review.md through [`crate::review_run`]). Its scout and reviewer sessions are read-only, and their process
//! trees go to the run like the implementer's, so a beat's `stop` cuts them. The reviewed commit and the requests it
//! made are kept for the run only.
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec};
use crate::review_context::{ReviewContext, piece_slice_description};
use crate::review_run::{StageInput, StageReview, review_lane};
use crate::reviewer::{Reviewer, ReviewerResult};
use crate::runs::{change_json, questions_json};
use crate::scouts::{Scouts, parse_scout_info};
use crate::tiers::mid_tier;

const REVIEWER_TEMPLATE: &str = include_str!("../../../prompts/reviewer.md");
const SCOUT_TEMPLATE: &str = include_str!("../../../prompts/scout.md");
const SCOUT_INFO: &str = include_str!("../../../prompts/scouts.json");

/// What the run remembers between its reviews.
#[derive(Default)]
pub(super) struct Reviews {
    /// Reviews that asked for changes so far.
    pub rounds: u32,
    /// The commit the last review saw, and what it asked for.
    pub last: Option<(String, String)>,
}

/// What a review decided for the run.
pub(super) enum Verdict {
    Approved,
    /// The requests to send the implementer.
    Changes(String),
    End(RunOutcome, Value),
}

/// The review's requests as the implementer reads them.
fn requests_text(r: &ReviewerResult) -> String {
    let mut out = r.summary.clone();
    for c in &r.comments {
        out += &match (&c.file, c.line) {
            (Some(f), Some(l)) => format!("\n- {f}:{l} — {}", c.body),
            (Some(f), None) => format!("\n- {f} — {}", c.body),
            _ => format!("\n- {}", c.body),
        };
    }
    out
}

/// A verdict as the run's next step; `pushed` is the change a person's question belongs to.
pub(super) fn judge(r: &ReviewerResult, pushed: &Pushed) -> Verdict {
    match r.verdict.as_str() {
        "approve" => Verdict::Approved,
        "request_changes" => Verdict::Changes(requests_text(r)),
        "needs_human" | "person_steps" if !r.questions.is_empty() => {
            let mut result = change_json(pushed);
            result["questions"] = questions_json(&r.questions);
            Verdict::End(RunOutcome::Questions, result)
        }
        "needs_human" | "person_steps" => {
            let mut why = format!("The review needs a person: {}", requests_text(r));
            for s in &r.person_steps {
                why += &format!("\n- {s}");
            }
            Verdict::End(RunOutcome::Failed(why), json!({}))
        }
        other => Verdict::End(
            RunOutcome::Failed(format!("The review gave the verdict {other}: {}", r.summary)),
            json!({}),
        ),
    }
}

pub(super) async fn head(cwd: &Path) -> Result<String> {
    let out = tokio::process::Command::new(crate::git_bin::program())
        .current_dir(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .await
        .context("reading HEAD")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Reviews the lane's change (all of it, or what changed since the last review) and posts the comment on the change.
pub(super) async fn review(
    job: &Job,
    cwd: &Path,
    pushed: &Pushed,
    implementer_summary: &str,
    checks: &str,
    reviews: &Reviews,
) -> Result<ReviewerResult> {
    let project = &job.project;
    let now = crate::telemetry::now_ms() / 1000;
    let tier = implementer_tier(None, now, &job.models);
    let sandbox = sandbox_enabled(project.sandbox, cfg!(windows));
    let session = |model: &str, budget_usd| EphemeralSessionConfig {
        cwd,
        budget_usd,
        model: model.to_string(),
        local: None,
        engine: Some(EngineId::Claude),
        cursor: None,
        sandbox,
        shell: true,
    };
    let info = parse_scout_info(SCOUT_INFO)?;
    let slice = piece_slice_description(&job.piece.what, &job.piece.done_when, implementer_summary);
    let (range, scope) = match &reviews.last {
        Some((sha, asked)) => (
            format!("{sha}..HEAD"),
            format!(
                "This is a re-review: read only what changed since the last one. It asked for:\n{asked}\nCheck that each request was dealt with."
            ),
        ),
        None => (format!("origin/{}...HEAD", project.base_branch), String::new()),
    };
    let id = piece_id(&job.piece);
    let input = StageInput {
        base_branch: &project.base_branch,
        review: project.review,
        common: ReviewContext {
            item_id: id,
            title: &job.piece.title,
            slice: 1,
            spec_refs: "(none)",
            slice_description: &slice,
            range: &range,
            scope: &scope,
            checks,
        },
        scouts: Scouts {
            template: SCOUT_TEMPLATE,
            session: session(mid_tier(tier), project.budget.scout_usd),
        },
        scout_info: &info,
        reviewer: Reviewer {
            template: REVIEWER_TEMPLATE,
            session: session(tier, project.budget.reviewer_usd),
        },
        playtest: None,
        manual_approvals: "(none)",
    };
    let cut = AtomicBool::new(false);
    let quiet = quiet_timeout(project.tuning.session_quiet_minutes);
    let run = |spec: SessionSpec| {
        let cut = &cut;
        async move {
            let interrupted = || anyhow::Error::new(Interrupted("the review was cut".into()));
            if cut.load(Ordering::SeqCst) {
                return Err(interrupted());
            }
            let engine = engine_of(spec.engine)?;
            let r: SessionResult =
                run_session_tracked(engine.as_ref(), &job.bin, &spec, "", None, quiet, Some(track(job))).await?;
            if r.subtype == "interrupted" {
                cut.store(true, Ordering::SeqCst);
                return Err(interrupted());
            }
            Ok(r)
        }
    };
    let StageReview { result, comment } = review_lane(cwd, &input, run).await?;
    let (gh, dir, number) = (job.gh.clone(), cwd.to_path_buf(), pushed.change.number.to_string());
    let posted = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(gh);
        cmd.current_dir(dir)
            .args(["pr", "comment", &number, "--body-file", "-"]);
        publish::run(cmd, Some(&comment), "gh pr comment")
    })
    .await;
    if let Err(e) = posted.map_err(anyhow::Error::from).and_then(|r| r) {
        tracing::warn!("{}: posting the review: {e:#}", job.token);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(v: serde_json::Value) -> ReviewerResult {
        serde_json::from_value(v).unwrap()
    }

    fn pushed() -> Pushed {
        Pushed {
            branch: "b".into(),
            change: publish::Published {
                url: "u".into(),
                number: 7,
                created: true,
            },
        }
    }

    #[test]
    fn needs_human_asks_when_the_reviewer_asked_else_fails_with_why() {
        let q =
            result(json!({"verdict":"needs_human","summary":"s","questions":[{"question":"Which?","options":["a"]}]}));
        let Verdict::End(RunOutcome::Questions, r) = judge(&q, &pushed()) else {
            panic!()
        };
        assert_eq!(r["questions"][0]["question"], "Which?");
        assert_eq!(r["change"]["number"], 7);
        let f = result(json!({"verdict":"person_steps","summary":"s","personSteps":["Add the key."]}));
        let Verdict::End(RunOutcome::Failed(why), _) = judge(&f, &pushed()) else {
            panic!()
        };
        assert!(why.contains("Add the key."), "{why}");
    }
}
