//! `checking` (`checkStep` in `stepsCi.ts`, agent/06.3-review.md stage 1): the project's checks run in the lane,
//! where main fails them too they don't count (`baseline`), and the item goes on to review, or back to its
//! implementer with what failed (`sendBack`, up to `maxCheckRounds`). The checks' evidence is kept for the review.
use std::sync::Mutex;

use super::Ctx;
use crate::checks::{CheckRun, StageOneResult, checks_summary};
use crate::implementer::publish;
use crate::item_lifecycle::{CheckOutcome, ItemState};
use crate::orchestrator::baseline_wait::BaselineStage;
use crate::orchestrator::session::ChecksStage;
use crate::orchestrator::steps::{StepDetail, StepDone, StepRequest};
use std::path::PathBuf;

/// `checksFailedMessage`.
fn failed_message(failed: &[&CheckRun]) -> String {
    let mut text = String::from("The project's checks failed on your branch:");
    for c in failed {
        let how = if c.timed_out { " (timed out)" } else { "" };
        text += &format!("\n\n### {}{how}\n```\n{}\n```", c.name, c.output_tail);
    }
    text + "\n\nFix them, commit, and return the result again."
}

/// The line `Checks: a ✓ b ✗` the item's history gets.
fn marks(result: &StageOneResult) -> String {
    let marks: Vec<String> = (result.checks.iter())
        .map(|c| format!("{} {}", c.name, if c.passed() { "✓" } else { "✗" }))
        .collect();
    format!(
        "Checks: {}",
        if marks.is_empty() {
            "none configured".into()
        } else {
            marks.join(" ")
        }
    )
}

pub(super) async fn step(ctx: Ctx, request: StepRequest) -> StepDone {
    let StepRequest {
        item_id,
        mut run,
        lane,
        branch,
        mut flow,
        item,
        ..
    } = request;
    // A chore's own differences (`chore_items::Flow`): a fix main is not forgiven what main fails too, a main sync is
    // not reviewed.
    let chore = item.as_ref().map(crate::chore_items::Flow::of).unwrap_or_default();
    let project = &ctx.project;
    let mut done = StepDone::default();
    // Checks the site set that this computer's owner hasn't approved are not "none configured": the item stays here
    // (fail closed) and the loop asks again, so an approval carries it on without a restart.
    if project.checks_unapproved {
        done.run = run;
        done.notes.push(
            "Waiting for the owner to approve the project's commands on this computer: its checks are set on the site \
             but not approved here, so none ran"
                .into(),
        );
        return done;
    }
    // How long main's results are waited for is the store's number; unknown, the item stays and the loop asks again.
    let Some(waits) = project
        .tuning
        .store
        .as_ref()
        .map(crate::orchestrator::baseline_wait::Waits::of)
    else {
        done.run = run;
        done.notes
            .push("Waiting for the store's numbers before the checks run".into());
        return done;
    };
    let Some(lane) = lane else {
        run.escalate();
        done.run = run;
        done.notes
            .push(format!("{item_id} is being checked with no lane to check it in"));
        return done;
    };
    let dir = match super::lane::ensure(&ctx, lane, branch.as_deref()).await {
        Ok(dir) => dir,
        Err(why) => {
            // The branch couldn't be put on the lane: the item stays here and the loop asks again.
            done.run = run;
            done.notes
                .push(format!("The lane couldn't be set to the branch: {why}"));
            return done;
        }
    };
    // The change the implementer opened, for the steps after this one (`prNumberOf`: found by its branch when the
    // record has none).
    if let (None, Some(branch)) = (&flow.pr_url, branch.clone()) {
        let (gh, found_in) = (ctx.gh.clone(), dir.clone());
        let found = tokio::task::spawn_blocking(move || publish::find_open(&gh, &found_in, &branch)).await;
        if let Ok(Ok(Some(change))) = found {
            flow.pr_url = Some(change.url);
            flow.pr_number = Some(change.number);
        }
    }
    // With CI only when ready, CI starts now, beside these checks and the review.
    super::ci::start(&ctx, &mut flow, &dir, &mut done.notes).await;
    // Checks a restart cut off may still be running in the lane (a cargo build holding its lock): stopped first.
    let stopping = dir.clone();
    let _ = tokio::task::spawn_blocking(move || crate::lanes::kill_processes_in(&stopping)).await;
    let result = Mutex::new(None);
    let (repo_path, worktrees_dir) = (PathBuf::from(&project.repo_path), PathBuf::from(&project.worktrees_dir));
    let project_id = project.id_or_derived();
    let stage = ChecksStage {
        checks: &project.checks,
        backlog_path: &project.backlog_path,
        on_tree: std::sync::Arc::new(|_| {}),
        result: &result,
        baseline: (!chore.no_baseline).then_some(BaselineStage {
            project: &project_id,
            repo_path: &repo_path,
            worktrees_dir: &worktrees_dir,
            checks: &project.checks,
            waits,
        }),
    };
    let mut notes = Vec::new();
    let ran = stage.run(&dir, &project.base_branch, &mut notes).await;
    let evidence = result.lock().unwrap().take();
    let Some(evidence) = evidence.filter(|_| ran.is_ok()) else {
        // The checks couldn't run (git, the lane): the item stays here and the loop asks again.
        let why = ran.err().map_or_else(String::new, |e| format!(": {e:#}"));
        done.run = run;
        done.notes.push(format!("The checks couldn't run{why}"));
        done.flow = Some(flow);
        return done;
    };
    done.notes.push(marks(&evidence));
    done.notes.extend(notes);
    done.detail = StepDetail {
        checks: evidence.check_times(),
        ..Default::default()
    };
    flow.checks_summary = Some(checks_summary(&evidence.checks));
    flow.diff_stat = Some(evidence.diff_stat.clone());
    let failed = evidence.blocking_failures();
    let outcome = evidence.outcome();
    run.checks_finished(outcome, project);
    if chore.merge_sync && run.state == ItemState::Reviewing {
        run.state = ItemState::Merging;
        done.notes
            .push("Checks passed; a main sync merges without a review".into());
    }
    match (outcome, run.state) {
        (CheckOutcome::Failed, ItemState::Implementing) => {
            let names: Vec<_> = failed.iter().map(|c| c.name.as_str()).collect();
            done.notes.push(format!(
                "Blocking checks failed: {}; sent back to the implementer",
                names.join(", ")
            ));
            done.send_back = Some(failed_message(&failed));
        }
        (CheckOutcome::Failed, _) => {
            let names: Vec<_> = failed.iter().map(|c| c.name.as_str()).collect();
            done.notes.push(format!(
                "Blocking checks still failing after {} rounds: {}",
                project.max_check_rounds,
                names.join(", ")
            ));
        }
        _ => {}
    }
    done.run = run;
    done.flow = Some(flow);
    done
}
