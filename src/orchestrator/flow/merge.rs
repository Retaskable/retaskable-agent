//! `merging` and `awaiting_merge` (`mergeStep`, `awaitMergeStep`, `landed` in the TS agent's `merge.ts`; agent/06.3-review.md "Merge",
//! github-workflow.md section 4). The loop holds the store's merge turn before it runs `merging` (`mergeQueueTurn`, `driver/item_merge.rs`)
//! and tells the store what the step found ([`MergeEnd`]); this reads the code host and does the git work. A change that landed ends
//! `merged`; one the base moved under is brought up to date (a clean merge goes back to CI, a conflict to the implementer, both counted
//! against `maxConflictRounds`); where GitHub or a person lands it the item waits in `awaiting_merge`. A merge GitHub isn't ready for
//! is tried again 30 seconds later, `maxMergeRetries` times.
use std::path::PathBuf;

use super::Ctx;
use super::ci::pr_of;
use super::host::{PrView, now_ms};
use crate::drafts::land::{Workflow, workflow};
use crate::item_lifecycle::{AwaitingMergeOutcome, ItemRun, ItemState, MergeOutcome};
use crate::merge_update::{Update, conflict_instruction, update_from_base};
use crate::orchestrator::merge_run::{not_ready, wants_update};
use crate::orchestrator::steps::{MergeEnd, StepDone, StepRequest};
use crate::store::FlowRecord;

/// How long a change may stay unworked-out by GitHub before the branch is brought up to date to make it look again.
const UNKNOWN_MS: i64 = 5 * 60_000;
const RETRY_MS: i64 = 30_000;
const POLL_MS: i64 = 15_000;

struct Ended {
    done: StepDone,
    run: ItemRun,
    flow: FlowRecord,
}

impl Ended {
    fn new(run: ItemRun, mut flow: FlowRecord) -> Self {
        flow.retry_after_ms = None;
        Self {
            done: StepDone::default(),
            run,
            flow,
        }
    }
    fn note(mut self, text: impl Into<String>) -> Self {
        self.done.notes.push(text.into());
        self
    }
    fn after(mut self, ms: i64) -> Self {
        self.flow.retry_after_ms = Some(now_ms() + ms);
        self
    }
    fn end(mut self, end: MergeEnd) -> Self {
        self.done.merge = Some(end);
        self
    }
    fn finish(mut self) -> StepDone {
        self.done.run = self.run;
        self.done.flow = Some(self.flow);
        self.done
    }
}

fn gh_in(ctx: &Ctx, args: Vec<String>) -> impl std::future::Future<Output = Result<(String, String, bool), String>> {
    let (gh, dir) = (ctx.gh.clone(), ctx.repo_dir());
    async move {
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            crate::ci_watch::gh_out(&gh, &dir, &refs, &|_| {}).map_err(|e| format!("{e:#}"))
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

fn needs_person(mut e: Ended, reason: String) -> StepDone {
    e.run.escalate();
    let end = MergeEnd::NeedsPerson { reason: reason.clone() };
    e.note(reason).end(end).finish()
}

/// The change landed: its merge commit is read (the view of a merged change has it), and the item is `merged`.
async fn landed(ctx: &Ctx, pr: u64, view: Option<PrView>, mut e: Ended) -> StepDone {
    let commit = match view.filter(|v| v.state == "MERGED") {
        Some(v) => v.merge_commit(),
        None => ctx.view(pr).await.ok().and_then(|v| v.merge_commit()),
    };
    match e.run.state {
        ItemState::AwaitingMerge => e.run.awaiting_merge_finished(AwaitingMergeOutcome::Merged),
        _ => e.run.merge_finished(MergeOutcome::Merged, &ctx.project),
    }
    (
        e.flow.land,
        e.flow.auto_merge_on,
        e.flow.merge_retries,
        e.flow.unknown_since,
    ) = (None, false, 0, None);
    e.note(format!("Merged pull request #{pr}"))
        .end(MergeEnd::Merged { commit })
        .finish()
}

/// Merges the base into the change's branch (a clean merge goes back to CI, a conflict to the implementer).
async fn bring_up_to_date(ctx: &Ctx, item: &str, branch: Option<&str>, mut e: Ended, why: String) -> StepDone {
    let (repo, worktrees, base) = (
        PathBuf::from(&ctx.project.repo_path),
        PathBuf::from(&ctx.project.worktrees_dir),
        ctx.project.base_branch.clone(),
    );
    let Some(branch) = branch.map(String::from) else {
        return needs_person(e, "Bringing the change up to date: the item has no branch.".into());
    };
    let piece = item.to_string();
    let b = base.clone();
    let done = tokio::task::spawn_blocking(move || {
        update_from_base(&crate::git_bin::command, &repo, &worktrees, &piece, &branch, &b)
    })
    .await;
    e.flow.unknown_since = None;
    match done {
        Ok(Ok(Update::Updated { head })) => {
            e.run
                .merge_finished(MergeOutcome::UpdatedFromBase { approval_kept: false }, &ctx.project);
            e.flow.ci_wait_since = Some(now_ms());
            e.flow.ci_reruns = 0;
            e.note(format!("{why}; checking it again before it merges"))
                .end(MergeEnd::Updated { head })
                .finish()
        }
        Ok(Ok(Update::Conflict { files })) => {
            e.run.merge_finished(MergeOutcome::Conflict, &ctx.project);
            let text = conflict_instruction(&base, &files);
            if e.run.state == ItemState::Implementing {
                e.done.send_back = Some(text.clone());
                e.note(format!("Merge conflict; sent back to merge {base} in"))
                    .end(MergeEnd::SentBack { reason: text })
                    .finish()
            } else {
                let reason = format!(
                    "Still conflicting with {base} after {} tries to bring it up to date (maxConflictRounds {}): other items keep merging into the same lines first.",
                    e.run.conflict_rounds.saturating_sub(1),
                    ctx.project.max_conflict_rounds
                );
                e.note(reason.clone()).end(MergeEnd::NeedsPerson { reason }).finish()
            }
        }
        other => {
            let why = match other {
                Ok(Err(err)) => format!("{err:#}"),
                Err(err) => err.to_string(),
                Ok(Ok(_)) => unreachable!(),
            };
            e.run.merge_finished(MergeOutcome::Failed, &ctx.project);
            let reason = format!("Bringing the change up to date failed: {why}");
            e.note(reason.clone()).end(MergeEnd::NeedsPerson { reason }).finish()
        }
    }
}

/// The project's land workflow; a main sync merges with a merge commit (a squash would make the next sync merge the same
/// commits again), whatever the project's method is (`merge.ts:285`).
fn chore_workflow(project: &crate::config::ProjectConfig, item: Option<&crate::backlog::Item>) -> Workflow {
    let mut wf = workflow(project);
    if item.is_some_and(|i| crate::chore_items::Flow::of(i).merge_sync) {
        wf.method_flag = "--merge";
    }
    wf
}

fn method(flag: &str) -> &str {
    flag
}

pub(super) async fn step(ctx: Ctx, request: StepRequest) -> StepDone {
    let StepRequest {
        item_id,
        run,
        branch,
        mut flow,
        item,
        ..
    } = request;
    let project = &ctx.project;
    let wf = chore_workflow(project, item.as_ref());
    let pr = match pr_of(&ctx, &mut flow, branch.as_deref()).await {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            let e = Ended::new(run, flow);
            return needs_person(
                e,
                "Lost track of this pull request: no PR number on record, and none found open for its branch.".into(),
            );
        }
        Err(_) => return Ended::new(run, flow).after(POLL_MS).finish(),
    };
    let mut e = Ended::new(run, flow);
    let Ok(view) = ctx.view(pr).await else {
        return e.after(POLL_MS).finish();
    };
    match view.state.as_str() {
        "MERGED" => return landed(&ctx, pr, Some(view), e).await,
        "CLOSED" => {
            e.run.merge_finished(MergeOutcome::Failed, project);
            e.done.merge = Some(MergeEnd::Closed);
            return e
                .note(format!("Pull request #{pr} was closed without merging."))
                .finish();
        }
        _ => {}
    }
    // Pushed since it was approved (a restart cut it between bringing it up to date and going to CI): that head goes through CI first.
    if let Some(approved) = e.flow.approved_sha.clone()
        && !view.head_ref_oid.is_empty()
        && view.head_ref_oid != approved
    {
        e.run
            .merge_finished(MergeOutcome::UpdatedFromBase { approval_kept: false }, project);
        e.flow.ci_wait_since = Some(now_ms());
        let head = view.head_ref_oid.clone();
        return e
            .note("Its pull request moved on since it was approved; checking it again before it merges")
            .end(MergeEnd::Updated { head })
            .finish();
    }
    let cap = project.max_conflict_rounds;
    if view.mergeable == "CONFLICTING" {
        if !e.run.may_update_from_base(project) {
            let reason = format!(
                "Still conflicting with {} after {} tries to bring it up to date (maxConflictRounds {cap}): other items keep merging into the same lines first.",
                project.base_branch, e.run.conflict_rounds
            );
            return needs_person(e, reason);
        }
        return bring_up_to_date(
            &ctx,
            &item_id,
            branch.as_deref(),
            e,
            "Merge conflict; merged the base in".into(),
        )
        .await;
    }
    if view.mergeable == "UNKNOWN" {
        let since = *e.flow.unknown_since.get_or_insert(now_ms());
        if now_ms() - since < UNKNOWN_MS {
            return e.after(POLL_MS).finish();
        }
        if !e.run.may_update_from_base(project) {
            let reason = format!(
                "GitHub has not worked out whether pull request #{pr} merges for over 5 minutes, and bringing it up to date {} times did not help.",
                e.run.conflict_rounds
            );
            return needs_person(e, reason);
        }
        let why = "GitHub has not worked out whether it merges for 5 minutes; merged the base in to make it look again"
            .to_string();
        return bring_up_to_date(&ctx, &item_id, branch.as_deref(), e, why).await;
    }
    e.flow.unknown_since = None;
    let mut args = vec!["pr".to_string(), "merge".into(), pr.to_string()];
    match wf.land.as_str() {
        "person" => {
            e.run.merge_finished(MergeOutcome::HandedToGithub, project);
            e.flow.land = Some("person".into());
            e.flow.auto_merge_on = false;
            let reason = "it waits for a person to merge it".to_string();
            let text = format!(
                "Pull request #{pr} is ready: waiting for you to merge it into {}",
                project.base_branch
            );
            return e.note(text).end(MergeEnd::SentBack { reason }).finish();
        }
        "auto_merge" | "queue" => {
            args.extend([method(wf.method_flag).to_string(), "--auto".into()]);
            if wf.delete_branch {
                args.push("--delete-branch".into());
            }
            let on = matches!(gh_in(&ctx, args).await, Ok((_, _, true)));
            e.run.merge_finished(MergeOutcome::HandedToGithub, project);
            e.flow.land = Some(wf.land.clone());
            e.flow.auto_merge_on = on;
            let text = if on {
                format!("Auto-merge is on for pull request #{pr}: GitHub merges it when the checks pass")
            } else {
                "GitHub didn't turn on auto-merge; trying again".to_string()
            };
            return e.note(text).end(MergeEnd::HandedOff).finish();
        }
        _ => {}
    }
    // Merged as it is, though the base moved since its checks ran: CI runs again only after a fix.
    args.push(method(wf.method_flag).to_string());
    if wf.delete_branch {
        args.push("--delete-branch".into());
    }
    match gh_in(&ctx, args).await {
        Ok((_, _, true)) => landed(&ctx, pr, None, e).await,
        Ok((_, err, false)) => {
            let behind = wants_update(&err) && e.run.may_update_from_base(project);
            if behind && !view.head_ref_oid.is_empty() {
                let why = format!(
                    "Behind {}, and GitHub wants it up to date; merged the base in",
                    project.base_branch
                );
                return bring_up_to_date(&ctx, &item_id, branch.as_deref(), e, why).await;
            }
            let (tries, max) = (e.flow.merge_retries + 1, project.tuning.max_merge_retries.min(20));
            if not_ready(&err) && tries <= max {
                e.flow.merge_retries = tries;
                let text = format!(
                    "GitHub wasn't ready to merge ({}); retrying ({tries}/{max})",
                    crate::text::truncate(err.trim(), 80)
                );
                return e.after(RETRY_MS).note(text).finish();
            }
            e.run.merge_finished(MergeOutcome::Failed, project);
            let reason = format!("gh pr merge failed: {}", err.trim());
            e.note(reason.clone()).end(MergeEnd::NeedsPerson { reason }).finish()
        }
        Err(_) => e.after(POLL_MS).finish(),
    }
}

/// `awaiting_merge`, each poll: merged goes on as a merge, closed needs a person, a conflict goes back to `merging`, and an open
/// change GitHub's auto-merge is meant to land has it turned on again if it is off.
pub(super) async fn awaiting(ctx: Ctx, request: StepRequest) -> StepDone {
    let StepRequest {
        run,
        branch,
        mut flow,
        item,
        ..
    } = request;
    let project = &ctx.project;
    let wf = chore_workflow(project, item.as_ref());
    let pr = match pr_of(&ctx, &mut flow, branch.as_deref()).await {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            return needs_person(
                Ended::new(run, flow),
                "Lost track of this pull request: no PR number on record, and none found open for its branch.".into(),
            );
        }
        Err(_) => return Ended::new(run, flow).after(POLL_MS).finish(),
    };
    let mut e = Ended::new(run, flow);
    let Ok(view) = ctx.view(pr).await else {
        return e.after(POLL_MS).finish();
    };
    match view.state.as_str() {
        "MERGED" => landed(&ctx, pr, Some(view), e).await,
        "CLOSED" => {
            e.run
                .awaiting_merge_finished(AwaitingMergeOutcome::ClosedWithoutMerging);
            e.done.merge = Some(MergeEnd::Closed);
            e.note(format!("Pull request #{pr} was closed without merging."))
                .finish()
        }
        // A conflict, or a change of who lands it, goes back to merging, which brings it up to date or lands it the new way.
        _ if view.mergeable == "CONFLICTING" || e.flow.land.as_deref().is_some_and(|l| l != wf.land) => {
            e.run.state = ItemState::Merging;
            (e.flow.land, e.flow.auto_merge_on) = (None, false);
            e.note(format!("Pull request #{pr} needs another look before it merges"))
                .finish()
        }
        _ if e.flow.land.as_deref() != Some("person") && !e.flow.auto_merge_on => {
            let mut args: Vec<String> = ["pr", "merge"].map(String::from).into();
            args.extend([pr.to_string(), method(wf.method_flag).into(), "--auto".into()]);
            let on = matches!(gh_in(&ctx, args).await, Ok((_, _, true)));
            e.flow.auto_merge_on = on;
            let e = if on {
                e.note(format!("Auto-merge is on for pull request #{pr}"))
            } else {
                e
            };
            e.after(RETRY_MS).finish()
        }
        _ => e.after(POLL_MS).finish(),
    }
}
