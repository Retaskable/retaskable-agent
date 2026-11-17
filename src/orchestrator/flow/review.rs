//! `reviewing` (`reviewStep` in `review.ts`, agent/06.3-review.md): an approval that still covers the head is reused;
//! otherwise the scouts and the final reviewer read the lane ([`crate::orchestrator::piece_review::run_lane_review`],
//! the same reading a piece's run makes), their comment goes on the change, and the verdict decides: approved goes
//! on (to the try-out when the reviewer or the diff's tier asks for one, else to CI or the merge), requested changes
//! go back to the implementer (up to `maxReviewRounds`), and a person's steps or a decision stop it.
use std::path::Path;

use super::Ctx;
use super::answer;
use crate::backlog::Item;
use crate::engines::{EngineId, Role, engine_for, place};
use crate::item_lifecycle::{ItemRun, ItemState, ReviewOutcome};
use crate::orchestrator::live::sessions::SessionInfo;
use crate::orchestrator::piece_approval::{Reuse, reuse};
use crate::orchestrator::piece_review::{Lane, ReviewEnv, Subject, post_comment, run_lane_review};
use crate::orchestrator::piece_tier;
use crate::orchestrator::steps::{StepDetail, StepDone, StepRequest};
use crate::reviewer::ReviewerResult;
use crate::store::{FlowRecord, TryOut};

/// How long a review that hit a usage limit waits before it is tried again.
const LIMIT_WAIT_MS: i64 = 10 * 60_000;

/// `reviewMessage`: what the implementer is sent back with.
fn review_message(result: &ReviewerResult, round: u32) -> String {
    let list: Vec<String> = (result.comments.iter())
        .map(|c| match (&c.file, c.line) {
            (Some(f), Some(l)) => format!("- {f}:{l} — {}", c.body),
            (Some(f), None) => format!("- {f} — {}", c.body),
            _ => format!("- {}", c.body),
        })
        .collect();
    format!(
        "The reviewer requested changes (round {round}):\n\n{}\n\n{}\n\nFix these, commit, and return the result again.",
        result.summary,
        list.join("\n")
    )
}

fn head(dir: &Path) -> Option<String> {
    crate::lanes::git_output_in_worktree(dir, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
}

/// What the item is told to the reviewer: the slice's description, or that none was given.
fn description(answer: Option<&answer::Answer>) -> String {
    match answer.map(|a| a.slice_description.trim()).filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => "(none given)".into(),
    }
}

/// The try-out a person is asked for: the plan's call wins (`auto` none, `manual` always), else the project's
/// `playtests` setting with what the reviewer asked for. A diff held for a check always asks, saying why.
fn planned_try_out(result: &ReviewerResult, answer: Option<&answer::Answer>, held: Option<&str>) -> Option<TryOut> {
    let asked = (result.playtest.as_ref())
        .filter(|p| !p.steps.is_empty())
        .map(|p| TryOut {
            what: p.what.clone(),
            steps: p.steps.clone(),
        });
    match held {
        Some(why) => {
            let base = asked
                .or_else(|| answer.and_then(|a| a.playtest.clone()))
                .unwrap_or(TryOut {
                    what: "Try it out".into(),
                    steps: vec![],
                });
            Some(TryOut {
                what: format!("{} (Held for a check: {why})", base.what),
                steps: base.steps,
            })
        }
        None => asked,
    }
}

pub(super) async fn step(ctx: Ctx, request: StepRequest) -> StepDone {
    let StepRequest {
        item_id,
        mut run,
        lane,
        branch,
        mut flow,
        item,
        risk_profile,
        ..
    } = request;
    let mut done = StepDone::default();
    let project = &ctx.project;
    let Some(lane) = lane else {
        run.escalate();
        done.run = run;
        done.notes
            .push(format!("{item_id} is being reviewed with no lane to review it in"));
        return done;
    };
    let dir = match super::lane::ensure(&ctx, lane, branch.as_deref()).await {
        Ok(dir) => dir,
        Err(why) => {
            done.run = run;
            done.notes
                .push(format!("The lane couldn't be set to the branch: {why}"));
            return done;
        }
    };
    let Some(sha) = head(&dir) else {
        // The lane can't be read: the item stays here and the loop asks again.
        done.run = run;
        done.notes.push("The review couldn't read the lane's head".into());
        return done;
    };
    let base_ref = format!("origin/{}", project.base_branch);
    // Approved at this head, or at one whose own changes read the same (a rebase, a conflict's small resolution).
    if let Some(why) = reuse(
        &dir,
        &base_ref,
        &project.backlog_path,
        &sha,
        flow.approved_sha.as_deref(),
        flow.conflict_of.as_deref(),
    ) {
        let note = match why {
            Reuse::SameHead => "Already approved at this commit; going on without another review".to_string(),
            Reuse::SameSlice => format!(
                "Rebased onto {} with the approved changes intact; going on without another review",
                project.base_branch
            ),
            Reuse::Resolution(n) => format!(
                "Merged {} in to settle a conflict, and the approved changes read the same but for {n} line(s) of the resolution; going on without another review",
                project.base_branch
            ),
        };
        run.review_finished(ReviewOutcome::Approved { playtest: false }, project);
        flow.reviewed_sha = Some(sha.clone());
        flow.approved_sha = Some(sha);
        flow.conflict_of = None;
        done.notes.push(note);
        done.run = run;
        done.flow = Some(flow);
        return done;
    }
    // Nothing starts while the agent is stopping or restarting: the review waits for the next one.
    if crate::process_tree::stopping() {
        done.run = run;
        return done;
    }
    let answer = answer::load(&ctx.data_dir, &project.id_or_derived(), &item_id);
    let item = item.unwrap_or_default();
    let reviewed = review(&ctx, &dir, lane, &item, &flow, answer.as_ref(), &sha).await;
    let lane_result = match reviewed {
        Err(e) => {
            done.run = run;
            done.notes.push(format!("The review couldn't run: {e:#}"));
            return done;
        }
        Ok(lane_result) => lane_result,
    };
    let review = match lane_result {
        Lane::Reviewed(r) => r,
        Lane::Interrupted => {
            done.run = run;
            return done;
        }
        Lane::Limited { engine, until } => {
            let wait = until.map_or(LIMIT_WAIT_MS, |d| d.as_millis() as i64);
            flow.retry_after_ms = Some(crate::telemetry::now_ms() + wait);
            done.notes.push(format!(
                "Hit {engine}'s usage limit during review; it will run again after"
            ));
            done.run = run;
            done.flow = Some(flow);
            return done;
        }
    };
    // The comment goes on the change first, as the TS agent posts it before it decides anything.
    if let Some(number) = flow.pr_number.or_else(|| find_number(&ctx, &dir, branch.as_deref())) {
        flow.pr_number = Some(number);
        let (gh, there, comment) = (ctx.gh.clone(), dir.clone(), review.comment.clone());
        let posted = tokio::task::spawn_blocking(move || post_comment(&gh, &there, number, &comment)).await;
        if let Ok(Err(e)) | Err(e) = posted.map_err(anyhow::Error::from) {
            done.notes.push(format!("Couldn't post the review comment: {e:#}"));
        }
    }
    let result = review.result;
    let round = run.review_rounds + 1;
    done.detail = StepDetail {
        lines: None,
        ..Default::default()
    };
    flow.reviewed_sha = Some(sha.clone());
    flow.retry_after_ms = None;
    match result.verdict.as_str() {
        "approve" => approved(
            &ctx,
            risk_profile.as_ref(),
            &dir,
            &item,
            &answer,
            &result,
            &sha,
            &mut run,
            &mut flow,
            &mut done,
        ),
        "request_changes" => {
            flow.approved_sha = None;
            let message = review_message(&result, round);
            flow.last_review = Some(message.clone());
            run.review_finished(ReviewOutcome::ChangesRequested, project);
            if run.state == ItemState::Implementing {
                done.notes.push(format!(
                    "Review: changes requested ({}); sent back to the implementer",
                    result.comments.len()
                ));
                done.send_back = Some(message);
            } else {
                done.notes.push(format!(
                    "The review still asked for changes after {} rounds",
                    project.max_review_rounds
                ));
            }
        }
        "person_steps" if !result.person_steps.is_empty() => {
            run.review_finished(ReviewOutcome::Escalated, project);
            let steps: Vec<String> = (result.person_steps.iter().enumerate())
                .map(|(i, s)| format!("{}. {s}", i + 1))
                .collect();
            done.notes
                .push(format!("Only steps a person can do are left:\n{}", steps.join("\n")));
        }
        _ => {
            run.review_finished(ReviewOutcome::Escalated, project);
            done.notes.push(format!("Reviewer: {}", result.summary));
        }
    }
    done.run = run;
    done.flow = Some(flow);
    done
}

/// The approval: the head is the approved one; where it goes is the tier's and the try-out's call.
#[allow(clippy::too_many_arguments)]
fn approved(
    ctx: &Ctx,
    risk_profile: Option<&crate::change_tier::RiskProfile>,
    dir: &Path,
    item: &Item,
    answer: &Option<answer::Answer>,
    result: &ReviewerResult,
    sha: &str,
    run: &mut ItemRun,
    flow: &mut FlowRecord,
    done: &mut StepDone,
) {
    let project = &ctx.project;
    flow.approved_sha = Some(sha.to_string());
    flow.last_review = None;
    // The real diff can raise the tier, never lower it: a change that touches sign-in, payments, secrets, commands, a
    // destructive migration or far more than its size is held for a person's check, with the reasons.
    let held = piece_tier::held(
        dir,
        &project.base_branch,
        &project.backlog_path,
        item.size.as_deref(),
        risk_profile,
    );
    let asked = planned_try_out(result, answer.as_ref(), held.as_deref());
    let wants_playtest = asked.is_some() && project.playtests != crate::config::Playtests::Off;
    run.review_finished(
        ReviewOutcome::Approved {
            playtest: wants_playtest,
        },
        project,
    );
    match (held, asked) {
        (Some(why), Some(try_out)) => {
            // Held: a person checks it whatever the project's setting says.
            run.state = ItemState::AwaitingPlaytest;
            flow.held = Some(why.clone());
            flow.playtest = Some(try_out);
            done.notes
                .push(format!("The change's diff raised its tier ({why}): a person checks it"));
        }
        (_, Some(try_out)) if run.state == ItemState::AwaitingPlaytest => {
            flow.playtest = Some(try_out);
            done.notes.push("Opus review: approved, playtest requested".into());
        }
        (_, Some(try_out)) if wants_playtest => {
            // `after_merge` (or `off` with steps given): not held, a person plays the merged build.
            flow.pending_playtest = Some(try_out);
            done.notes
                .push("Opus review: approved; playtest deferred until after it merges".into());
        }
        _ => done.notes.push("Opus review: approved".into()),
    }
}

/// The open change's number, found by the branch when the record has none.
fn find_number(ctx: &Ctx, dir: &Path, branch: Option<&str>) -> Option<u64> {
    let change = crate::implementer::publish::find_open(&ctx.gh, dir, branch?).ok()??;
    Some(change.number)
}

/// The scouts and the reviewer on the lane's diff: all of it the first time, only what changed since after a round.
async fn review(
    ctx: &Ctx,
    dir: &Path,
    lane: u32,
    item: &Item,
    flow: &FlowRecord,
    answer: Option<&answer::Answer>,
    sha: &str,
) -> anyhow::Result<Lane> {
    let project = &ctx.project;
    let since = flow.reviewed_sha.as_deref().filter(|s| {
        *s != sha && crate::lanes::git_output_in_worktree(dir, &["merge-base", "--is-ancestor", s, "HEAD"]).is_ok()
    });
    let (range, scope) = match (since, &flow.last_review) {
        (Some(s), asked) => (
            format!("{s} HEAD"),
            format!(
                "This is a re-review: an earlier round already reviewed this slice up to {s}{}, so look only at what changed since (`git diff {s} HEAD`).",
                asked.as_ref().map_or(String::new(), |a| format!(
                    " and asked for:\n{a}\nCheck each requested change was made"
                ))
            ),
        ),
        _ => (format!("origin/{}...HEAD", project.base_branch), String::new()),
    };
    let chosen = engine_for(&ctx.choice, project.engines.as_ref(), Role::Reviewer, None);
    let placed = place(chosen, Role::Reviewer, &ctx.env, &ctx.cursor_models).map_err(anyhow::Error::msg)?;
    let row = |role: Role, engine: EngineId, model: String| SessionInfo {
        project: project.id_or_derived(),
        item_id: item.id.clone(),
        title: Some(item.title.clone()),
        role,
        // A scout holds no lane.
        lane: (role != Role::Scout).then_some(lane),
        engine,
        model,
    };
    let env = ReviewEnv {
        project,
        choice: &ctx.choice,
        env: &ctx.env,
        cursor_models: &ctx.cursor_models,
        bin: &placed.bin,
        local: &ctx.local,
        sessions: &ctx.sessions,
        context: None,
        label: &item.id,
        row: &row,
        tree: None,
    };
    let checks = flow
        .checks_summary
        .clone()
        .unwrap_or_else(|| "(no checks configured)".into());
    let spec_refs = if item.spec_refs.is_empty() {
        "(none)".to_string()
    } else {
        item.spec_refs.join(", ")
    };
    let description = description(answer);
    let subject = Subject {
        item_id: &item.id,
        title: &item.title,
        slice: item.slice,
        spec_refs: &spec_refs,
        description: &description,
        checks: &checks,
        playtest: None,
    };
    run_lane_review(&env, dir, &subject, &range, &scope).await
}
