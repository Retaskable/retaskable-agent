//! Review inside a `build` or `fix` run (docs/specs/pieces.md section 3.2 with agent/06.3-review.md stage 3's
//! verdicts): once the change is pushed and open, the scouts and final reviewer read the lane
//! ([`crate::review_run::review_lane`]), their sessions registered with the run so a beat's `stop` cuts them, and
//! the reviewer's comment goes on the change with `gh pr comment`. What was reviewed is remembered for the run
//! only, so a re-review reads what changed since.
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::piece_runs::{Job, piece_item, tree_sender};
use super::session::watch_for;
use crate::backlog::runs::RunOutcome;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Placement, Role, SessionResult, SessionSpec, engine_for, engine_of, place,
    run_session_tracked,
};
use crate::implementer::publish::run as run_command;
use crate::implementer::{Interrupted, Playtest};
use crate::local_model::{LocalModel, LocalSlot};
use crate::orchestrator::live::sessions::model_name;
use crate::review_context::{ReviewContext, piece_slice_description};
use crate::review_run::{StageInput, review_lane};
use crate::reviewer::{Reviewer, ReviewerResult};
use crate::runs::Pushed;
use crate::scouts::{Scouts, parse_scout_info};
use crate::tiers::{LOCAL, OPUS, SONNET, scout_tier, top_tier};

const SCOUT_TEMPLATE: &str = include_str!("../../prompts/scout.md");
const SCOUT_INFO: &str = include_str!("../../prompts/scouts.json");
const REVIEWER_TEMPLATE: &str = include_str!("../../prompts/reviewer.md");

/// What a run remembers between its reviews.
#[derive(Default)]
pub(super) struct ReviewState {
    /// The commit the last review saw, and what it asked for.
    reviewed: Option<(String, String)>,
    /// Reviews that asked for changes.
    rounds: u32,
}

/// What a review decided for the run.
pub(super) enum Reviewed {
    /// The run ends `pr_opened`.
    Approved,
    /// The implementer is resumed with these requests.
    Changes(String),
    /// The run ends here.
    Ends(RunOutcome, Value),
}

fn ends(outcome: RunOutcome) -> Reviewed {
    Reviewed::Ends(outcome, json!({}))
}

fn head(cwd: &Path) -> Option<String> {
    crate::lanes::git_output_in_worktree(cwd, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
}

/// Whether `sha` is still behind `HEAD` (a rebase makes it not).
fn behind_head(cwd: &Path, sha: &str) -> bool {
    crate::lanes::git_output_in_worktree(cwd, &["merge-base", "--is-ancestor", sha, "HEAD"]).is_ok()
}

/// Where a review role runs: its `review` group's engine with `engine-placement` On, else the Claude the run
/// started with.
fn placement(job: &Job, role: Role) -> Result<Placement> {
    let engine = if job.placed {
        job.context
            .engine_now(engine_for(&job.choice, job.project.engines.as_ref(), role, None))
    } else {
        crate::config::Engine::Claude
    };
    place(engine, role, &job.env, &job.cursor_models).map_err(anyhow::Error::msg)
}

/// The local model's slot for a review session, if it runs there: a session already set to `local` waits for the
/// slot; a scout whose tier isn't `local` takes it when it is free right now (and then runs `local`), else
/// carries on at once. Cursor never takes it. The slot is held until the returned value is dropped.
async fn claim_local(
    local: &LocalModel,
    ladder: bool,
    program: EngineId,
    role: Role,
    spec: &mut SessionSpec,
) -> Option<LocalSlot> {
    if !ladder || program != EngineId::Claude {
        None
    } else if spec.model == LOCAL {
        Some(local.take().await)
    } else if role == Role::Scout && local.available() {
        local.try_take().inspect(|_| {
            spec.model = LOCAL.to_string();
            spec.local = Some(local.target());
        })
    } else {
        None
    }
}

async fn run_session(
    job: &Job,
    bins: &[&Placement],
    ladder: bool,
    role: Role,
    mut spec: SessionSpec,
) -> Result<SessionResult> {
    let program = spec.engine.unwrap_or(EngineId::Claude);
    let bin = bins.iter().find(|p| p.engine == program).map_or(&job.bin, |p| &p.bin);
    let engine = engine_of(Some(program))?;
    let quiet = watch_for(&job.project, program);
    let _slot = claim_local(&job.local, ladder, program, role, &mut spec).await;
    let mut info = job.session_info(role, program, model_name(program, &spec.model, spec.cursor.as_ref()));
    // A scout holds no lane.
    if role == Role::Scout {
        info.lane = None;
    }
    let run = run_session_tracked(engine.as_ref(), bin, &spec, "", None, quiet, Some(tree_sender(job)));
    let result = job.sessions.track(info, run).await?;
    // A cut session (a stop) isn't a failed review: the run ends `interrupted`.
    if result.subtype == "interrupted" {
        return Err(Interrupted("the review session was cut".into()).into());
    }
    Ok(result)
}

fn session_config<'a>(
    job: &Job,
    cwd: &'a Path,
    model: &str,
    budget_usd: f64,
    placed: &Placement,
) -> EphemeralSessionConfig<'a> {
    EphemeralSessionConfig {
        cwd,
        budget_usd,
        model: model.to_string(),
        local: (model == LOCAL).then(|| job.local.target()),
        engine: Some(placed.engine),
        cursor: placed.cursor.clone(),
        sandbox: crate::engines::claude::sandbox_enabled(job.project.sandbox, cfg!(windows)),
        shell: true,
    }
}

/// The requests the implementer is told to deal with.
fn requests(result: &ReviewerResult) -> String {
    let list = result
        .comments
        .iter()
        .map(|c| match (&c.file, c.line) {
            (Some(f), Some(l)) => format!("- {f}:{l} — {}", c.body),
            (Some(f), None) => format!("- {f} — {}", c.body),
            _ => format!("- {}", c.body),
        })
        .collect::<Vec<_>>()
        .join("\n");
    if list.is_empty() {
        result.summary.clone()
    } else {
        format!("{}\n\n{list}", result.summary)
    }
}

fn post_comment(gh: &Path, cwd: &Path, number: u64, comment: &str) -> Result<()> {
    let mut cmd = Command::new(gh);
    cmd.current_dir(cwd)
        .args(["pr", "comment", &number.to_string(), "--body-file", "-"]);
    run_command(cmd, Some(comment), "gh pr comment").map(|_| ())
}

/// Reviews the lane's change, posts the comment and says what happens next. A failure to review is `Err`: the
/// caller ends the run `failed`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn review(
    job: &Job,
    cwd: &Path,
    pushed: &Pushed,
    slice_description: &str,
    playtest: Option<&Playtest>,
    checks: &str,
    state: &mut ReviewState,
    usage: Option<&Value>,
) -> Result<Reviewed> {
    let project = &job.project;
    let item = piece_item(&job.piece);
    let description = piece_slice_description(&job.piece.what, &job.piece.done_when, slice_description);
    let sha = head(cwd).context("reading the lane's HEAD")?;
    let since = state.reviewed.as_ref().filter(|(s, _)| behind_head(cwd, s));
    let (range, scope) = match since {
        Some((s, asked)) => (
            format!("{s} HEAD"),
            format!(
                "This is a re-review. The last review saw {s} and asked for:\n{asked}\nCheck those are done; read only what changed since (`git diff {s} HEAD`)."
            ),
        ),
        None => (format!("origin/{}...HEAD", project.base_branch), String::new()),
    };
    let (scout_at, reviewer_at) = (placement(job, Role::Scout)?, placement(job, Role::Reviewer)?);
    let info = parse_scout_info(SCOUT_INFO)?;
    // With `local-tier-ladder` Off, scouts run on Sonnet and the reviewer on Opus.
    let ladder = job.flags.on("local-tier-ladder").await;
    let (scout_model, top_model) = if ladder {
        let now = crate::telemetry::now_ms() / 1000;
        let up = |p: &Placement| job.local.available() && p.engine == EngineId::Claude;
        (
            scout_tier(usage, now, &job.models, up(&scout_at)),
            top_tier(usage, now, &job.models, up(&reviewer_at)),
        )
    } else {
        (SONNET, OPUS)
    };
    let input = StageInput {
        base_branch: &project.base_branch,
        review: project.review,
        common: ReviewContext {
            item_id: &item.id,
            title: &job.piece.title,
            slice: 1,
            spec_refs: "(none)",
            slice_description: &description,
            range: &range,
            scope: &scope,
            checks,
        },
        scouts: Scouts {
            template: SCOUT_TEMPLATE,
            session: session_config(job, cwd, scout_model, project.budget.scout_usd, &scout_at),
        },
        scout_info: &info,
        reviewer: Reviewer {
            template: REVIEWER_TEMPLATE,
            session: session_config(job, cwd, top_model, project.budget.reviewer_usd, &reviewer_at),
        },
        playtest,
        manual_approvals: "(none)",
    };
    let bins = [&scout_at, &reviewer_at];
    let review = review_lane(cwd, &input, |role, spec| run_session(job, &bins, ladder, role, spec)).await;
    let review = match review {
        Ok(r) => r,
        Err(e) if e.downcast_ref::<Interrupted>().is_some() => {
            return Ok(ends(RunOutcome::Interrupted));
        }
        Err(e) => return Err(e),
    };
    let (gh, dir, number, comment) = (job.gh.clone(), cwd.to_path_buf(), pushed.change.number, review.comment);
    tokio::task::spawn_blocking(move || post_comment(&gh, &dir, number, &comment))
        .await
        .context("posting the review")?
        .context("posting the review comment")?;
    let result = review.result;
    Ok(match result.verdict.as_str() {
        "approve" => Reviewed::Approved,
        "request_changes" => {
            state.rounds += 1;
            let asked = requests(&result);
            if state.rounds > project.max_review_rounds {
                Reviewed::Ends(
                    RunOutcome::Failed(format!(
                        "The review still asked for changes after {} rounds: {asked}",
                        project.max_review_rounds
                    )),
                    json!({}),
                )
            } else {
                state.reviewed = Some((sha, asked.clone()));
                Reviewed::Changes(format!(
                    "The review asked for changes. Make them, commit, and answer again:\n\n{asked}"
                ))
            }
        }
        "needs_human" | "person_steps" if !result.questions.is_empty() => {
            let mut asked = crate::runs::change_json(pushed);
            asked["questions"] = crate::runs::questions_json(&result.questions);
            Reviewed::Ends(RunOutcome::Questions, asked)
        }
        "needs_human" | "person_steps" => {
            let steps = if result.person_steps.is_empty() {
                String::new()
            } else {
                format!(" Steps: {}", result.person_steps.join("; "))
            };
            ends(RunOutcome::Failed(format!(
                "The review needs a person: {}{steps}",
                result.summary
            )))
        }
        other => ends(RunOutcome::Failed(format!(
            "The review gave an unknown verdict, {other}"
        ))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine::test_spec;

    fn up() -> LocalModel {
        let local = LocalModel::new(crate::config::LocalModelConfig {
            enabled: true,
            base_url: "http://127.0.0.1:1".into(),
            model: "m".into(),
        });
        local.set_up(true);
        local
    }

    async fn claim(
        local: &LocalModel,
        ladder: bool,
        program: EngineId,
        role: Role,
        model: &str,
    ) -> (SessionSpec, bool) {
        let mut spec = test_spec(Path::new("."), "p");
        spec.model = model.into();
        let slot = claim_local(local, ladder, program, role, &mut spec).await;
        (spec, slot.is_some())
    }

    #[tokio::test]
    async fn one_scout_takes_the_free_slot_and_the_rest_stay_on_sonnet_without_waiting() {
        let local = up();
        let mut first = test_spec(Path::new("."), "p");
        first.model = SONNET.into();
        let held = claim_local(&local, true, EngineId::Claude, Role::Scout, &mut first).await;
        assert!(held.is_some());
        assert_eq!(first.model, LOCAL);
        assert_eq!(first.local, Some(local.target()));
        // The first still holds the slot while the others start.
        let mut held = test_spec(Path::new("."), "p");
        held.model = SONNET.into();
        let slot = claim_local(&local, true, EngineId::Claude, Role::Scout, &mut held).await;
        assert!(slot.is_none());
        assert_eq!((held.model.as_str(), held.local), (SONNET, None));
    }

    #[tokio::test]
    async fn scouts_stay_on_sonnet_with_local_down_off_the_flag_or_on_cursor() {
        let off = LocalModel::off();
        for (local, ladder, program) in [
            (&off, true, EngineId::Claude),
            (&up(), false, EngineId::Claude),
            (&up(), true, EngineId::Cursor),
        ] {
            let (spec, took) = claim(local, ladder, program, Role::Scout, SONNET).await;
            assert_eq!((spec.model.as_str(), spec.local, took), (SONNET, None, false));
        }
    }

    #[tokio::test]
    async fn the_reviewer_never_takes_the_slot_unless_its_tier_is_local() {
        let local = up();
        let (spec, took) = claim(&local, true, EngineId::Claude, Role::Reviewer, OPUS).await;
        assert_eq!((spec.model.as_str(), took), (OPUS, false));
        let (spec, took) = claim(&local, true, EngineId::Claude, Role::Reviewer, LOCAL).await;
        assert_eq!((spec.model.as_str(), took), (LOCAL, true));
    }
}
