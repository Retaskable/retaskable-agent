//! The code gates' rounds before a push, shared by the claim and the piece runs.
use std::path::Path;

use anyhow::{Context, Result};

use super::{Attempt, ran_out_of_turns};
use crate::engines::EngineId;
use crate::implementer::code_gates;
use crate::orchestrator::fresh_start::{FreshStart, fresh_for_engine};

/// What the code gates' rounds need to know of a run.
pub(in crate::orchestrator) struct GateRounds<'a> {
    pub cwd: &'a Path,
    pub base_branch: &'a str,
    pub code_gates: crate::config::CodeGates,
    pub max_check_rounds: u32,
    /// The claim's fresh-start rules (06.2-implementer.md "Fresh-start rules on resume"); `None` for a run that
    /// only ever resumes.
    pub fresh: Option<FreshRules>,
    /// Stage 1 of review (06.3-review.md): the lane's checks, run once the gates pass; `None` for the claim.
    pub checks: Option<&'a ChecksStage<'a>>,
}

/// The checks a piece run puts its change through, and where their result is kept for the run's end.
pub(in crate::orchestrator) struct ChecksStage<'a> {
    pub checks: &'a [crate::config::CheckConfig],
    pub backlog_path: &'a str,
    pub on_tree: std::sync::Arc<dyn Fn(std::sync::Arc<crate::process_tree::ProcessTree>) + Send + Sync>,
    pub result: &'a std::sync::Mutex<Option<crate::checks::StageOneResult>>,
    /// Tells a failure that main has too from the change's own.
    pub baseline: Option<crate::orchestrator::baseline_wait::BaselineStage<'a>>,
}

impl ChecksStage<'_> {
    /// Runs the checks in `cwd`; a blocking failure is `(names of the failing checks, the message to send back)`.
    /// With a baseline, a failure that main has too is marked `preexisting` and doesn't count; the notes say what
    /// the comparison did.
    pub(super) async fn run(
        &self,
        cwd: &Path,
        base: &str,
        notes: &mut Vec<String>,
    ) -> Result<Option<(String, String)>> {
        let (dir, base_owned) = (cwd.to_path_buf(), base.to_string());
        let (checks, backlog, on_tree) = (
            self.checks.to_vec(),
            self.backlog_path.to_string(),
            self.on_tree.clone(),
        );
        let stage = tokio::task::spawn_blocking(move || {
            crate::checks::run_registered(&dir, &base_owned, &checks, &backlog, &*on_tree)
        })
        .await
        .context("running the checks")?
        .context("running the checks")?;
        let mut stage = stage;
        if let (Some(baseline), false) = (&self.baseline, stage.blocking_failures().is_empty()) {
            notes.extend(baseline.mark(cwd, base, &mut stage.checks).await);
        }
        let failures: Vec<_> = stage.blocking_failures().into_iter().cloned().collect();
        *self.result.lock().unwrap() = Some(stage);
        if failures.is_empty() {
            return Ok(None);
        }
        let names = failures.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ");
        let mut message = String::from("Before Retaskable pushes your branch, its checks failed:");
        for c in &failures {
            let how = if c.timed_out {
                "timed out".to_string()
            } else {
                format!("exit code {}", c.exit_code)
            };
            message += &format!(
                "\n\n`{}` ({how}), the end of its output:\n```\n{}\n```",
                c.command, c.output_tail
            );
        }
        Ok(Some((names, message)))
    }
}

/// When a session is replaced instead of resumed.
#[derive(Clone, Copy)]
pub(in crate::orchestrator) struct FreshRules {
    pub max_turns: u32,
    pub max_resume_context_tokens: u32,
    /// The engine the session started on, and the lane's own engine now (if it has one).
    pub engine: EngineId,
    pub lane_engine: Option<EngineId>,
    /// The engine that takes over while the one the session started on is at its usage limit.
    pub takeover: Option<EngineId>,
}

/// What [`gate_rounds`] asks its caller to run next.
pub(in crate::orchestrator) enum Step {
    /// The session ran out of turns: a fresh one carries on in the same lane, uncommitted work kept.
    OutOfTurns,
    /// Send the findings back: resume the session.
    Fix(String),
    /// Send the findings to a fresh session, the old one having grown too large to resume.
    /// ...and the engine it runs on (`None` without fresh-start rules).
    FixFresh(String, Option<EngineId>),
}

/// How [`gate_rounds`] ended.
pub(in crate::orchestrator) enum Gated {
    /// The answer to settle, with the gates' notes.
    Settled(Attempt, Vec<String>),
    /// A person is needed, for this reason.
    GaveUp(Attempt, Vec<String>, String),
}

/// Code gates before the push (06.2-implementer.md): while the session answers `pr_opened`, the gates check the
/// lane's change against `origin/<baseBranch>`. A finding goes back through `next` and its answer is gated too;
/// every such round counts against `maxCheckRounds`, and past it a person is needed. Gates that can't run (a git
/// error) are noted and the answer stands. With fresh-start rules, running out of turns restarts once. Shared by
/// the claim and the piece runs.
pub(in crate::orchestrator) async fn gate_rounds<F, Fut>(
    g: &GateRounds<'_>,
    mut a: Attempt,
    mut next: F,
) -> Result<Gated>
where
    F: FnMut(Step) -> Fut,
    Fut: std::future::Future<Output = Result<Attempt>>,
{
    let mut notes = Vec::new();
    let mut rounds = 0;
    let mut restarted = false;
    let mut current_engine = g.fresh.map(|f| f.engine);
    loop {
        if let Some(fresh) = g.fresh.filter(|_| ran_out_of_turns(&a)) {
            if restarted {
                let why = format!("Implementer ran out of turns ({}) twice in a row", fresh.max_turns);
                return Ok(Gated::GaveUp(a, notes, why));
            }
            restarted = true;
            notes.push(FreshStart::OutOfTurns.history_line());
            a = next(Step::OutOfTurns).await?;
            continue;
        }
        if !matches!(&a.answer, Ok(r) if r.status == "pr_opened") {
            break;
        }
        let (dir, base, gates) = (g.cwd.to_path_buf(), g.base_branch.to_string(), g.code_gates);
        let found = tokio::task::spawn_blocking(move || code_gates::run(&dir, &base, &gates))
            .await
            .context("running the code gates")?;
        let found = match found {
            Ok(found) => found.map(|f| ("code gates", f.note, f.message)),
            Err(e) => {
                notes.push(format!("Code gates couldn't run, so the push goes ahead: {e:#}"));
                None
            }
        };
        let found = match (found, g.checks) {
            (Some(f), _) => f,
            (None, None) => break,
            (None, Some(stage)) => match stage.run(g.cwd, g.base_branch, &mut notes).await? {
                Some((names, message)) => ("checks", names, message),
                None => break,
            },
        };
        let (what, note, message) = found;
        notes.push(note.clone());
        rounds += 1;
        if rounds > g.max_check_rounds {
            let why = format!(
                "The {what} still found problems after {} rounds: {note}",
                g.max_check_rounds
            );
            return Ok(Gated::GaveUp(a, notes, why));
        }
        let follow_up = format!("{}\n\nFix them, commit, and return the result again.", message);
        // A session resumes only on the engine it started on, and not past `maxResumeContextTokens`.
        let reason = g.fresh.and_then(|f| {
            // The takeover was picked for the engine the session started on, so it only counts until a fresh start moves it.
            let takeover = f.takeover.filter(|_| current_engine.is_none_or(|e| e == f.engine));
            fresh_for_engine(current_engine.unwrap_or(f.engine), f.lane_engine, takeover).or_else(|| {
                a.context_tokens
                    .filter(|t| *t > u64::from(f.max_resume_context_tokens))
                    .map(FreshStart::Grown)
            })
        });
        a = match reason {
            Some(reason) => {
                notes.push(reason.history_line());
                if let Some(f) = g.fresh {
                    let takeover = reason_takeover(&reason);
                    current_engine = Some(takeover.or(f.lane_engine).unwrap_or(current_engine.unwrap_or(f.engine)));
                }

                next(Step::FixFresh(follow_up, current_engine)).await?
            }
            None => next(Step::Fix(follow_up)).await?,
        };
    }
    Ok(Gated::Settled(a, notes))
}

/// The engine a fresh start for `reason` moves to for being at its usage limit.
pub(in crate::orchestrator) fn reason_takeover(reason: &FreshStart) -> Option<EngineId> {
    match reason {
        FreshStart::EngineLimited { to, .. } => Some(*to),
        _ => None,
    }
}
