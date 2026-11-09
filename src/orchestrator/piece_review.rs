//! Review inside a `build` or `fix` run (docs/specs/pieces.md section 3.2 with agent/06.3-review.md stage 3's
//! verdicts): once the change is pushed and open, the scouts and final reviewer read the lane
//! ([`crate::review_run::review_lane`]), their sessions registered with the run so a beat's `stop` cuts them, and
//! the reviewer's comment goes on the change with `gh pr comment`. What was reviewed is remembered for the run
//! only, so a re-review reads what changed since.
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::piece_runs::{Job, piece_item, tree_sender};
use super::session::watch_for;
use crate::backlog::runs::RunOutcome;
use crate::engines::cursor::with_project;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Placement, Role, SessionResult, SessionSpec, engine_for, engine_of, place,
    program_of, run_session_tracked, start_engine,
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
fn placement(job: &Job, role: Role, hit: &BTreeSet<EngineId>) -> Result<Placement> {
    let engine = if job.placed {
        job.context
            .engine_now_out(engine_for(&job.choice, job.project.engines.as_ref(), role, None), hit)
    } else {
        crate::config::Engine::Claude
    };
    place(engine, role, &job.env, &job.cursor_models).map_err(anyhow::Error::msg)
}

/// The models the scouts and the final reviewer start on: Sonnet and Opus with the ladder Off, else `scout_tier` and
/// `top_tier`, each given whether the local model can run that role (`up`: scouts, reviewer).
fn review_models(
    ladder: bool,
    usage: Option<&Value>,
    now: i64,
    models: &crate::config::ModelsConfig,
    up: (bool, bool),
    at_limit: bool,
) -> (&'static str, &'static str) {
    if ladder {
        // Claude at its limit reads as the floor for a role the model can run (`local-limit-fallback`).
        let (s, t) = (
            crate::tiers::limit_floor(usage, at_limit, up.0),
            crate::tiers::limit_floor(usage, at_limit, up.1),
        );
        (
            scout_tier(s.as_ref(), now, models, up.0),
            top_tier(t.as_ref(), now, models, up.1),
        )
    } else {
        (SONNET, OPUS)
    }
}

/// The local model's slot for a review session, if it runs there: with `ladder`, a session already set to `local`
/// waits for the slot; with `grab` (`local-chores-scouts`), a scout whose tier isn't `local` takes it when it is free
/// right now (and then runs `local`), else carries on at once. Cursor never takes it. The slot is held until the returned value is dropped.
async fn claim_local(
    local: &LocalModel,
    (grab, ladder): (bool, bool),
    program: EngineId,
    role: Role,
    spec: &mut SessionSpec,
) -> Option<LocalSlot> {
    if program != EngineId::Claude {
        None
    } else if ladder && spec.model == LOCAL {
        Some(local.take().await)
    } else if grab && spec.model != LOCAL && role == Role::Scout && local.available() {
        local.try_take().inspect(|_| {
            spec.model = LOCAL.to_string();
            spec.local = Some(local.target());
        })
    } else {
        None
    }
}

/// The engine a review session ended at the limit of, and how long until it lifts, when the usage reading said.
type Hit = Mutex<Option<(EngineId, Option<Duration>)>>;

async fn run_session(
    job: &Job,
    bins: &[&Placement],
    hit: &Hit,
    local_flags: (bool, bool),
    role: Role,
    mut spec: SessionSpec,
) -> Result<SessionResult> {
    // A review that is going to re-run starts nothing more.
    if hit.lock().unwrap().is_some() {
        anyhow::bail!("an engine hit its usage limit; the review re-runs");
    }
    let program = spec.engine.unwrap_or(EngineId::Claude);
    let bin = bins.iter().find(|p| p.engine == program).map_or(&job.bin, |p| &p.bin);
    let engine = engine_of(Some(program))?;
    let quiet = watch_for(&job.project, program);
    let _slot = claim_local(&job.local, local_flags, program, role, &mut spec).await;
    let mut info = job.session_info(role, program, model_name(program, &spec.model, spec.cursor.as_ref()));
    // A scout holds no lane.
    if role == Role::Scout {
        info.lane = None;
    }
    let run = run_session_tracked(engine.as_ref(), bin, &spec, "", None, quiet, Some(tree_sender(job)));
    let result = job.sessions.track(info, run).await?;
    // With `engine-fall-over` On, a session at its engine's limit sends the whole review to another engine.
    if result.limit_hit && job.placed && job.context.fall.is_some() {
        let wait = crate::engines::engine::resets_in(result.usage.as_ref(), crate::tiers::weekly_reading_on());
        hit.lock().unwrap().get_or_insert((program, wait));
    }
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
        cursor: placed.cursor.clone().map(|c| with_project(&job.project, c)),
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
    let number = number.to_string();
    let args = ["pr", "comment", &number, "--body-file", "-"];
    crate::host_call::guard("gh", cwd, &args)?;
    let mut cmd = Command::new(gh);
    cmd.current_dir(cwd).args(args);
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
    let mut out = BTreeSet::new();
    let review = loop {
        let (scout_at, reviewer_at) = (
            placement(job, Role::Scout, &out)?,
            placement(job, Role::Reviewer, &out)?,
        );
        let info = parse_scout_info(SCOUT_INFO)?;
        // With `local-tier-ladder` Off, scouts run on Sonnet and the reviewer on Opus.
        let ladder = job.flags.on("local-tier-ladder").await;
        let grab = job.flags.on("local-chores-scouts").await;
        let up = |p: &Placement| job.local.available() && p.engine == EngineId::Claude;
        let (scout_model, top_model) = review_models(
            ladder,
            usage,
            crate::telemetry::now_ms() / 1000,
            &job.models,
            (up(&scout_at), up(&reviewer_at)),
            job.sessions.claude_at_limit(),
        );
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
        let hit = Hit::default();
        let review = review_lane(cwd, &input, |role, spec| {
            run_session(job, &bins, &hit, (grab, ladder), role, spec)
        })
        .await;
        if let Err(e) = &review
            && e.downcast_ref::<Interrupted>().is_some()
        {
            return Ok(ends(RunOutcome::Interrupted));
        }
        let Some((engine, until)) = hit.into_inner().unwrap() else {
            break review?;
        };
        // The engine is out now: the whole review re-runs on the one that takes over, once per limit hit.
        out.insert(engine);
        let next = job
            .context
            .engine_now_out(start_engine(Some(engine), crate::config::Engine::Claude), &out);
        if program_of(next) == engine {
            let until = until
                .and_then(|d| chrono::Duration::from_std(d).ok())
                .and_then(|d| chrono::Utc::now().checked_add_signed(d));
            return Ok(ends(RunOutcome::Limited {
                engine: engine.to_string(),
                until,
            }));
        }
        tracing::info!(
            "{}: Hit {engine}'s usage limit; carrying on now with {}",
            job.token,
            program_of(next)
        );
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
        flags: (bool, bool),
        program: EngineId,
        role: Role,
        model: &str,
    ) -> (SessionSpec, bool) {
        let mut spec = test_spec(Path::new("."), "p");
        spec.model = model.into();
        let slot = claim_local(local, flags, program, role, &mut spec).await;
        (spec, slot.is_some())
    }

    #[tokio::test]
    async fn one_scout_takes_the_free_slot_and_the_rest_stay_on_sonnet_without_waiting() {
        let local = up();
        let mut first = test_spec(Path::new("."), "p");
        first.model = SONNET.into();
        let held = claim_local(&local, (true, false), EngineId::Claude, Role::Scout, &mut first).await;
        assert!(held.is_some());
        assert_eq!(first.model, LOCAL);
        assert_eq!(first.local, Some(local.target()));
        // The first still holds the slot while the others start.
        let mut held = test_spec(Path::new("."), "p");
        held.model = SONNET.into();
        let slot = claim_local(&local, (true, false), EngineId::Claude, Role::Scout, &mut held).await;
        assert!(slot.is_none());
        assert_eq!((held.model.as_str(), held.local), (SONNET, None));
    }

    #[tokio::test]
    async fn scouts_stay_on_sonnet_with_local_down_off_the_flag_or_on_cursor() {
        let off = LocalModel::off();
        for (local, flags, program) in [
            (&off, (true, true), EngineId::Claude),
            (&up(), (false, false), EngineId::Claude),
            (&up(), (false, true), EngineId::Claude), // the ladder alone never grabs the free slot
            (&up(), (true, true), EngineId::Cursor),
            (&up(), (true, false), EngineId::Codex),
        ] {
            let (spec, took) = claim(local, flags, program, Role::Scout, SONNET).await;
            assert_eq!((spec.model.as_str(), spec.local, took), (SONNET, None, false));
        }
    }

    #[tokio::test]
    async fn a_scout_takes_the_free_slot_with_either_ladder_setting_when_the_flag_is_on() {
        for ladder in [false, true] {
            let local = up();
            let (spec, took) = claim(&local, (true, ladder), EngineId::Claude, Role::Scout, SONNET).await;
            assert_eq!((spec.model.as_str(), took), (LOCAL, true));
        }
    }

    #[tokio::test]
    async fn the_reviewer_never_takes_the_free_slot_and_waits_only_when_its_tier_is_local_on_the_ladder() {
        let local = up();
        let (spec, took) = claim(&local, (true, true), EngineId::Claude, Role::Reviewer, OPUS).await;
        assert_eq!((spec.model.as_str(), took), (OPUS, false));
        let (spec, took) = claim(&local, (false, true), EngineId::Claude, Role::Reviewer, LOCAL).await;
        assert_eq!((spec.model.as_str(), took), (LOCAL, true));
        let (_, took) = claim(&up(), (true, false), EngineId::Claude, Role::Reviewer, LOCAL).await;
        assert!(!took);
    }

    fn reading(utilization: f64) -> Value {
        json!({"unifiedWindows": {"five_hour": {"utilization": utilization, "resetsAt": 1_000_100}}})
    }

    #[test]
    fn the_reviewer_is_local_only_below_the_pause_line_and_only_when_local_can_run_it() {
        let m = crate::config::ModelsConfig::default();
        let now = 1_000_000;
        let (low, full) = (reading(0.99), reading(0.1));
        assert_eq!(review_models(true, Some(&low), now, &m, (true, true), false).1, LOCAL);
        assert_eq!(review_models(true, Some(&low), now, &m, (true, false), false).1, OPUS); // a Cursor reviewer
        assert_eq!(
            review_models(true, Some(&low), now, &m, (false, false), false),
            (SONNET, OPUS)
        ); // local down
        assert_eq!(
            review_models(true, Some(&full), now, &m, (true, true), false),
            (SONNET, OPUS)
        );
        assert_eq!(
            review_models(false, Some(&low), now, &m, (true, true), false),
            (SONNET, OPUS)
        ); // flag Off
    }
}
