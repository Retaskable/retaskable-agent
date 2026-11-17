//! Review inside a `build` or `fix` run (docs/specs/pieces.md section 3.2 with agent/06.3-review.md stage 3's
//! verdicts): once the change is pushed and open, the scouts and final reviewer read the lane
//! ([`crate::review_run::review_lane`]), their sessions registered with the run so a beat's `stop` cuts them, and
//! the reviewer's comment goes on the change with `gh pr comment`. What was reviewed is remembered for the run
//! only, so a re-review reads what changed since.
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::piece_approval::reuse;
use super::piece_runs::{Job, piece_item, tree_sender};
use super::session::watch_for;
use crate::backlog::runs::RunOutcome;
use crate::config::{CursorModels, EngineChoiceConfig, ProjectConfig};
use crate::engines::cursor::with_project;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Placement, Role, SessionResult, SessionSpec, engine_for, engine_of, place,
    program_of, run_session_tracked, start_engine,
};
use crate::env::Env;
use crate::implementer::publish::run as run_command;
use crate::implementer::{Interrupted, Playtest};
use crate::local_model::{LocalModel, LocalSlot};
use crate::orchestrator::driver::RunContext;
use crate::orchestrator::live::sessions::model_name;
use crate::orchestrator::live::sessions::{SessionInfo, Sessions};
use crate::review_context::{ReviewContext, piece_slice_description};
use crate::review_run::{StageInput, review_lane};
use crate::reviewer::{Reviewer, ReviewerResult};
use crate::runs::Pushed;
use crate::scouts::{Scouts, parse_scout_info};
use crate::tiers::{LOCAL, OPUS, SONNET, floor};

const SCOUT_TEMPLATE: &str = include_str!("../../prompts/scout.md");
const SCOUT_INFO: &str = include_str!("../../prompts/scouts.json");
const REVIEWER_TEMPLATE: &str = include_str!("../../prompts/reviewer.md");

/// What a review's sessions run with: the project, the engine choice, this computer's binaries and the rows its
/// sessions list themselves as. A piece's run ([`Job::review_env`]) and a leased item's step both make one.
pub(super) struct ReviewEnv<'a> {
    pub project: &'a ProjectConfig,
    pub choice: &'a EngineChoiceConfig,
    pub env: &'a Env,
    pub cursor_models: &'a CursorModels,
    pub bin: &'a Path,
    pub local: &'a LocalModel,
    pub sessions: &'a Sessions,
    /// Where an engine at its limit hands the review to the one that takes over (a run's context); `None` places the
    /// sessions as chosen and a limit hit ends the review at once.
    pub context: Option<&'a RunContext>,
    /// What the review's log lines call the work.
    pub label: &'a str,
    /// The row a session lists itself as: its role, engine and model.
    pub row: &'a (dyn Fn(Role, EngineId, String) -> SessionInfo + Send + Sync),
    /// A sender for a session's process tree, so a run's `stop` can cut it.
    pub tree: Option<
        &'a (dyn Fn() -> tokio::sync::oneshot::Sender<std::sync::Arc<crate::process_tree::ProcessTree>> + Send + Sync),
    >,
}

/// What a review reads about the change.
pub(super) struct Subject<'a> {
    pub item_id: &'a str,
    pub title: &'a str,
    pub slice: u32,
    pub spec_refs: &'a str,
    pub description: &'a str,
    pub checks: &'a str,
    pub playtest: Option<&'a Playtest>,
}

/// How the scouts and the reviewer ended.
pub(super) enum Lane {
    Reviewed(crate::review_run::StageReview),
    /// A session was cut (a stop).
    Interrupted,
    /// An engine is at its limit and nothing takes over: when it lifts, if said.
    Limited {
        engine: EngineId,
        until: Option<Duration>,
    },
}

/// The scouts and the final reviewer on the lane's diff ([`review_lane`]), run on the engines the roles are set to; an
/// engine at its limit re-runs the whole review on the one that takes over, once per hit.
pub(super) async fn run_lane_review(
    env: &ReviewEnv<'_>,
    cwd: &Path,
    subject: &Subject<'_>,
    range: &str,
    scope: &str,
) -> Result<Lane> {
    let project = env.project;
    let mut out = BTreeSet::new();
    loop {
        let (scout_at, reviewer_at) = (
            placement(env, Role::Scout, &out)?,
            placement(env, Role::Reviewer, &out)?,
        );
        let info = parse_scout_info(SCOUT_INFO)?;
        let up = |p: &Placement| env.local.available() && p.engine == EngineId::Claude;
        let (scout_model, top_model) = review_models((up(&scout_at), up(&reviewer_at)), env.sessions.claude_at_limit());
        let input = StageInput {
            base_branch: &project.base_branch,
            review: project.review,
            common: ReviewContext {
                item_id: subject.item_id,
                title: subject.title,
                slice: subject.slice,
                spec_refs: subject.spec_refs,
                slice_description: subject.description,
                range,
                scope,
                checks: subject.checks,
            },
            scouts: Scouts {
                template: SCOUT_TEMPLATE,
                session: session_config(env, cwd, scout_model, project.budget.scout_usd, &scout_at),
            },
            scout_info: &info,
            reviewer: Reviewer {
                template: REVIEWER_TEMPLATE,
                session: session_config(env, cwd, top_model, project.budget.reviewer_usd, &reviewer_at),
            },
            playtest: subject.playtest,
            manual_approvals: "(none)",
        };
        let bins = [&scout_at, &reviewer_at];
        let hit = Hit::default();
        let review = review_lane(cwd, &input, |role, spec| run_session(env, &bins, &hit, role, spec)).await;
        if let Err(e) = &review
            && e.downcast_ref::<Interrupted>().is_some()
        {
            return Ok(Lane::Interrupted);
        }
        let Some((engine, until)) = hit.into_inner().unwrap() else {
            return Ok(Lane::Reviewed(review?));
        };
        // The engine is out now: the whole review re-runs on the one that takes over.
        out.insert(engine);
        let chosen = start_engine(Some(engine), crate::config::Engine::Claude);
        let next = env.context.map_or(chosen, |c| c.engine_now_out(chosen, &out));
        if program_of(next) == engine {
            return Ok(Lane::Limited { engine, until });
        }
        tracing::info!(
            "{}: Hit {engine}'s usage limit; carrying on now with {}",
            env.label,
            program_of(next)
        );
    }
}

impl Job {
    /// What this run's review sessions run with.
    pub(super) fn review_env<'a>(
        &'a self,
        row: &'a (dyn Fn(Role, EngineId, String) -> SessionInfo + Send + Sync),
        tree: &'a (
                dyn Fn() -> tokio::sync::oneshot::Sender<std::sync::Arc<crate::process_tree::ProcessTree>> + Send + Sync
            ),
    ) -> ReviewEnv<'a> {
        ReviewEnv {
            project: &self.project,
            choice: &self.choice,
            env: &self.env,
            cursor_models: &self.cursor_models,
            bin: &self.bin,
            local: &self.local,
            sessions: &self.sessions,
            context: Some(&self.context),
            label: &self.token,
            row,
            tree: Some(tree),
        }
    }
}

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
    /// The run ends `pr_opened`, saying this head (`result.approved`) is one a review approved: its own, or an earlier
    /// one's reused ([`super::piece_approval`]).
    Approved(String),
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

/// Where a review role runs: its `review` group's engine.
fn placement(env: &ReviewEnv<'_>, role: Role, hit: &BTreeSet<EngineId>) -> Result<Placement> {
    let chosen = engine_for(env.choice, env.project.engines.as_ref(), role, None);
    let engine = env.context.map_or(chosen, |c| c.engine_now_out(chosen, hit));
    place(engine, role, env.env, env.cursor_models).map_err(anyhow::Error::msg)
}

/// The models the scouts (`sonnet`) and the final reviewer (`opus`) start on, each given whether the local model can run that role (`up`: scouts, reviewer). Claude at its limit reads as the floor for a role the model can run.
fn review_models(up: (bool, bool), at_limit: bool) -> (&'static str, &'static str) {
    (floor(SONNET, at_limit, up.0), floor(OPUS, at_limit, up.1))
}

/// The local model's slot for a review session, if it runs there: a session already set to `local`
/// waits for the slot; a scout whose tier isn't `local` takes it when it is free
/// right now (and then runs `local`), else carries on at once. Cursor never takes it. The slot is held until the returned value is dropped.
async fn claim_local(local: &LocalModel, program: EngineId, role: Role, spec: &mut SessionSpec) -> Option<LocalSlot> {
    if program != EngineId::Claude {
        None
    } else if spec.model == LOCAL {
        Some(local.take().await)
    } else if spec.model != LOCAL && role == Role::Scout && local.available() {
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
    env: &ReviewEnv<'_>,
    bins: &[&Placement],
    hit: &Hit,
    role: Role,
    mut spec: SessionSpec,
) -> Result<SessionResult> {
    // A review that is going to re-run starts nothing more.
    if hit.lock().unwrap().is_some() {
        anyhow::bail!("an engine hit its usage limit; the review re-runs");
    }
    let program = spec.engine.unwrap_or(EngineId::Claude);
    let bin = bins.iter().find(|p| p.engine == program).map_or(env.bin, |p| &p.bin);
    let engine = engine_of(Some(program))?;
    let quiet = watch_for(env.project, program);
    let _slot = claim_local(env.local, program, role, &mut spec).await;
    let mut info = (env.row)(role, program, model_name(program, &spec.model, spec.cursor.as_ref()));
    // A scout holds no lane.
    if role == Role::Scout {
        info.lane = None;
    }
    let run = run_session_tracked(
        engine.as_ref(),
        bin,
        &spec,
        "",
        None,
        quiet,
        env.tree.as_ref().map(|t| t()),
    );
    let result = env.sessions.track(info, run).await?;
    // A session at its engine's limit sends the whole review to another engine.
    if result.limit_hit && env.context.is_some_and(|c| c.fall.is_some()) {
        let wait = crate::engines::engine::resets_in(result.usage.as_ref());
        hit.lock().unwrap().get_or_insert((program, wait));
    }
    // A cut session (a stop) isn't a failed review: the run ends `interrupted`.
    if result.subtype == "interrupted" {
        return Err(Interrupted("the review session was cut".into()).into());
    }
    Ok(result)
}

fn session_config<'a>(
    env: &ReviewEnv<'_>,
    cwd: &'a Path,
    model: &str,
    budget_usd: f64,
    placed: &Placement,
) -> EphemeralSessionConfig<'a> {
    EphemeralSessionConfig {
        cwd,
        budget_usd,
        model: model.to_string(),
        local: (model == LOCAL).then(|| env.local.target()),
        engine: Some(placed.engine),
        cursor: placed.cursor.clone().map(|c| with_project(env.project, c)),
        sandbox: crate::engines::claude::sandbox_enabled(env.project.sandbox, cfg!(windows)),
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

pub(super) fn post_comment(gh: &Path, cwd: &Path, number: u64, comment: &str) -> Result<()> {
    let number = number.to_string();
    let mut cmd = crate::spawn::command(gh);
    cmd.current_dir(cwd)
        .args(["pr", "comment", &number, "--body-file", "-"]);
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
) -> Result<Reviewed> {
    let project = &job.project;
    let item = piece_item(&job.piece);
    let description = piece_slice_description(&job.piece.what, &job.piece.done_when, slice_description);
    let sha = head(cwd).context("reading the lane's HEAD")?;
    // The run's first review only: a later round is of changes this run's reviewer asked for.
    if state.reviewed.is_none()
        && let Some(why) = reuse(
            cwd,
            &format!("origin/{}", project.base_branch),
            &project.backlog_path,
            &sha,
            job.piece.approved_sha.as_deref(),
            job.piece.conflict_of.as_deref(),
        )
    {
        tracing::info!("{}: {sha} needs no new review ({why:?})", job.token);
        return Ok(Reviewed::Approved(sha));
    }
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
    let (row, tree) = (
        |role, engine, model| job.session_info(role, engine, model),
        || tree_sender(job),
    );
    let env = job.review_env(&row, &tree);
    let subject = Subject {
        item_id: &item.id,
        title: &job.piece.title,
        slice: 1,
        spec_refs: "(none)",
        description: &description,
        checks,
        playtest,
    };
    let review = match run_lane_review(&env, cwd, &subject, &range, &scope).await? {
        Lane::Reviewed(review) => review,
        Lane::Interrupted => return Ok(ends(RunOutcome::Interrupted)),
        Lane::Limited { engine, until } => {
            let until = until
                .and_then(|d| chrono::Duration::from_std(d).ok())
                .and_then(|d| chrono::Utc::now().checked_add_signed(d));
            return Ok(ends(RunOutcome::Limited {
                engine: engine.to_string(),
                until,
            }));
        }
    };
    let (gh, dir, number, comment) = (job.gh.clone(), cwd.to_path_buf(), pushed.change.number, review.comment);
    tokio::task::spawn_blocking(move || post_comment(&gh, &dir, number, &comment))
        .await
        .context("posting the review")?
        .context("posting the review comment")?;
    let result = review.result;
    Ok(match result.verdict.as_str() {
        "approve" => Reviewed::Approved(sha),
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
        "needs_human" | "person_steps" if !result.questions.is_empty() => Reviewed::Ends(
            RunOutcome::Questions,
            crate::runs::questions_end(Some(pushed), &result.questions),
        ),
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
    use crate::tiers::{OPUS, SONNET};

    fn up() -> LocalModel {
        let local = LocalModel::new(crate::config::LocalModelConfig {
            enabled: true,
            base_url: "http://127.0.0.1:1".into(),
            model: "m".into(),
        });
        local.set_up(true);
        local
    }

    async fn claim(local: &LocalModel, program: EngineId, role: Role, model: &str) -> (SessionSpec, bool) {
        let mut spec = test_spec(Path::new("."), "p");
        spec.model = model.into();
        let slot = claim_local(local, program, role, &mut spec).await;
        (spec, slot.is_some())
    }

    #[tokio::test]
    async fn one_scout_takes_the_free_slot_and_the_rest_stay_on_sonnet_without_waiting() {
        let local = up();
        let mut first = test_spec(Path::new("."), "p");
        first.model = SONNET.into();
        let held = claim_local(&local, EngineId::Claude, Role::Scout, &mut first).await;
        assert!(held.is_some());
        assert_eq!(first.model, LOCAL);
        assert_eq!(first.local, Some(local.target()));
        // The first still holds the slot while the others start.
        let mut held = test_spec(Path::new("."), "p");
        held.model = SONNET.into();
        let slot = claim_local(&local, EngineId::Claude, Role::Scout, &mut held).await;
        assert!(slot.is_none());
        assert_eq!((held.model.as_str(), held.local), (SONNET, None));
    }

    #[tokio::test]
    async fn scouts_stay_on_sonnet_with_local_down_or_on_cursor() {
        let off = LocalModel::off();
        for (local, program) in [
            (&off, EngineId::Claude),
            (&up(), EngineId::Cursor),
            (&up(), EngineId::Codex),
        ] {
            let (spec, took) = claim(local, program, Role::Scout, SONNET).await;
            assert_eq!((spec.model.as_str(), spec.local, took), (SONNET, None, false));
        }
    }

    #[tokio::test]
    async fn a_scout_takes_the_free_slot_when_local_is_up() {
        let local = up();
        let (spec, took) = claim(&local, EngineId::Claude, Role::Scout, SONNET).await;
        assert_eq!((spec.model.as_str(), took), (LOCAL, true));
    }

    #[tokio::test]
    async fn the_reviewer_never_takes_the_free_slot_and_waits_only_when_its_tier_is_local() {
        let local = up();
        let (spec, took) = claim(&local, EngineId::Claude, Role::Reviewer, OPUS).await;
        assert_eq!((spec.model.as_str(), took), (OPUS, false));
        let (spec, took) = claim(&local, EngineId::Claude, Role::Reviewer, LOCAL).await;
        assert_eq!((spec.model.as_str(), took), (LOCAL, true));
    }

    #[test]
    fn the_reviewer_is_local_only_at_the_limit_and_only_when_local_can_run_it() {
        assert_eq!(review_models((true, true), true), (LOCAL, LOCAL));
        assert_eq!(review_models((true, false), true), (LOCAL, OPUS)); // a Cursor reviewer
        assert_eq!(review_models((false, false), true), (SONNET, OPUS)); // local down
        assert_eq!(review_models((true, true), false), (SONNET, OPUS));
    }
}
