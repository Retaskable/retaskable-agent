//! `awaiting_ci` (`pollCi` in `stepsCi.ts`, `rerunCi`, the label's start, agent/06.3-review.md "CI", ci-when-ready.md): the
//! item's change is read on the code host each poll. Passing goes on as the lifecycle says (merge for a change CI ran on
//! at its approved head, the checks or the review otherwise); a red goes to the implementer with each failed check's log
//! (`maxCheckRounds`); a run that was only cancelled, or a check pending past `ciMaxMinutes`, is run again (`maxCiReruns`);
//! no CI for the base, or none showing up within `ciGraceMinutes`, carries on. With `ciWhenReady` the `ci-ready` label goes
//! on beside the checks ([`start`]) and only a check that ended after it counts. A step reads `gh`, never the store.
use std::path::Path;

use super::Ctx;
use super::host::now_ms;
use crate::ci_checks::{CommitState, WHAT_MAX, ci_logs, commit_checks, ended_at, failed_note};
use crate::ci_coverage::{CiAnswer, covering, has_ci_file, read_workflows};
use crate::ci_pass::host::CiHost;
use crate::implementer::publish;
use crate::item_lifecycle::{CiOutcome, ItemRun, ItemState, ci_when_ready_active};
use crate::orchestrator::steps::{StepDone, StepRequest};
use crate::store::FlowRecord;

/// How long before CI is read again while it runs (the watch run's own poll).
const POLL_MS: i64 = 15_000;
/// How old a change is before its label goes on (`NEW_CHANGE_MS`): GitHub sends "opened" and "labeled" for a change labelled
/// as it opens, and CI read the label on both and ran twice on the same commit.
const NEW_CHANGE_MS: i64 = 20_000;
/// How long [`start`] waits for the host to show a push as the change's head (`PUSH_SEEN_MS`), and how often it asks.
const PUSH_SEEN_MS: u64 = 10_000;
const PUSH_SEEN_EVERY_MS: u64 = 1_000;

fn words(minutes: u32) -> String {
    match minutes {
        1 => "1 minute".into(),
        m if m % 60 == 0 => format!("{} hour{}", m / 60, if m == 60 { "" } else { "s" }),
        m => format!("{m} minutes"),
    }
}

/// The change opened for the item: the record's, else the open one found by its branch (`prNumberOf`). `Err` when the host
/// couldn't be asked; `Ok(None)` when none is open.
pub(super) async fn pr_of(ctx: &Ctx, flow: &mut FlowRecord, branch: Option<&str>) -> Result<Option<u64>, String> {
    if let Some(n) = flow.pr_number {
        return Ok(Some(n));
    }
    let Some(branch) = branch.map(String::from) else {
        return Ok(None);
    };
    let (gh, dir) = (ctx.gh.clone(), ctx.repo_dir());
    match tokio::task::spawn_blocking(move || publish::find_open(&gh, &dir, &branch)).await {
        Ok(Ok(Some(change))) => {
            flow.pr_url = Some(change.url);
            flow.pr_number = Some(change.number);
            Ok(Some(change.number))
        }
        Ok(Ok(None)) => Ok(None),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether CI runs for changes into the base (`noCiNote`): read from the workflows on the base branch.
async fn covered(ctx: &Ctx) -> bool {
    let (repo, base) = (ctx.repo_dir(), ctx.project.base_branch.clone());
    let answer = tokio::task::spawn_blocking(move || {
        let at = format!("origin/{base}");
        covering(&read_workflows(&repo, &at), &base, has_ci_file(&repo, &at)).answer
    })
    .await;
    !matches!(answer, Ok(CiAnswer::NotCovered | CiAnswer::None))
}

/// With CI only when ready, the label goes on as the change goes to the lane's checks, once the host shows the push as the
/// change's head and the change is [`NEW_CHANGE_MS`] old, so CI runs beside the checks and the review (`startCi`). Best
/// effort: when the host doesn't answer the checks go on and `awaiting_ci` puts the label on once the change is approved.
pub(super) async fn start(ctx: &Ctx, flow: &mut FlowRecord, lane: &Path, notes: &mut Vec<String>) {
    let (Some(pr), None) = (flow.pr_number, flow.ci_ready_at) else {
        return;
    };
    if !ci_when_ready_active(&ctx.project) || !ctx.project.on_github() || !covered(ctx).await {
        return;
    }
    let pushed = crate::lanes::head_sha(lane).ok();
    let mut waited = 0;
    let mut seen = ctx.view(pr).await.ok();
    while let (Some(sha), Some(v)) = (&pushed, &seen) {
        if &v.head_ref_oid == sha || waited >= PUSH_SEEN_MS {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(PUSH_SEEN_EVERY_MS)).await;
        waited += PUSH_SEEN_EVERY_MS;
        seen = ctx.view(pr).await.ok();
    }
    if let Some(age) = seen.as_ref().and_then(|v| v.age_ms(now_ms()))
        && (0..NEW_CHANGE_MS).contains(&age)
    {
        tokio::time::sleep(std::time::Duration::from_millis((NEW_CHANGE_MS - age) as u64)).await;
    }
    let host = ctx.ci_host();
    match tokio::task::spawn_blocking(move || host.add_label(pr)).await {
        Ok(Ok(())) => {
            let now = now_ms();
            flow.ci_ready_at = Some(now);
            flow.ci_wait_since = Some(now);
            notes.push(format!(
                "Added the ci-ready label to pull request #{pr}: CI runs beside the checks and review"
            ));
        }
        other => {
            let why = match other {
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => e.to_string(),
                Ok(Ok(())) => unreachable!(),
            };
            notes.push(format!(
                "Couldn't add the ci-ready label ({}); it goes on once the change is approved",
                crate::text::truncate(&why, 120)
            ));
        }
    }
}

/// The step ends with the item staying in `awaiting_ci`: asked again after the poll wait.
/// [`stay`] after a read that failed: the reason is noted (cut short) so a stall explains itself.
fn stay_failed(mut done: StepDone, run: ItemRun, flow: FlowRecord, what: &str, why: &str) -> StepDone {
    tracing::warn!("ci step: {what}: {why}");
    done.notes.push(format!(
        "{what}: {}; trying again",
        crate::text::truncate(why.trim(), 200)
    ));
    stay(done, run, flow)
}

fn stay(mut done: StepDone, run: ItemRun, mut flow: FlowRecord) -> StepDone {
    flow.retry_after_ms = Some(now_ms() + POLL_MS);
    done.run = run;
    done.flow = Some(flow);
    done
}

pub(super) async fn step(ctx: Ctx, request: StepRequest) -> StepDone {
    let StepRequest {
        mut run,
        branch,
        mut flow,
        item,
        ..
    } = request;
    let merge_sync = item
        .as_ref()
        .is_some_and(|i| crate::chore_items::Flow::of(i).merge_sync);
    let project = &ctx.project;
    let mut done = StepDone::default();
    flow.retry_after_ms = None;
    if !project.ci_required() {
        run.state = ItemState::Checking;
        done.run = run;
        done.flow = Some(flow);
        return done;
    }
    // The waits and the reruns are the store's numbers (`health`'s `tuning`); unknown, CI is not read and the poll goes on.
    let Some(t) = project.tuning.store else {
        return stay(done, run, flow);
    };
    let pr = match pr_of(&ctx, &mut flow, branch.as_deref()).await {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            run.escalate();
            done.run = run;
            done.flow = Some(flow);
            done.notes.push(
                "Lost track of this pull request: no PR number on record, and none found open for its branch.".into(),
            );
            return done;
        }
        Err(why) => return stay_failed(done, run, flow, "Couldn't look up its pull request", &why),
    };
    let when_ready = ci_when_ready_active(project);
    let now = now_ms();
    // No workflow runs for changes into the base (or this host reads no CI): not waited for.
    if !project.on_github() || !covered(&ctx).await {
        let note = format!("No CI runs for pull requests into {}", project.base_branch);
        return carry_on(&ctx, done, run, flow, pr, note).await;
    }
    if when_ready && flow.ci_ready_at.is_none() {
        // Not checked and approved yet (here from before the setting was on): the lane's checks first, where CI starts beside them.
        if flow.approved_sha.is_none() && !merge_sync {
            run.state = ItemState::Checking;
            done.notes
                .push("Not checked and approved yet; running the checks here, with CI beside them".into());
            done.run = run;
            done.flow = Some(flow);
            return done;
        }
        // Approved with no label on (adding it beside the checks failed): it goes on now.
        let host = ctx.ci_host();
        if !matches!(
            tokio::task::spawn_blocking(move || host.add_label(pr)).await,
            Ok(Ok(()))
        ) {
            return stay(done, run, flow);
        }
        flow.ci_ready_at = Some(now);
        flow.ci_wait_since = Some(now);
        done.notes
            .push(format!("Added the ci-ready label to pull request #{pr}: CI runs now"));
        return stay(done, run, flow);
    }
    let view = match ctx.view(pr).await {
        Ok(view) => view,
        Err(why) => return stay_failed(done, run, flow, &format!("Couldn't read pull request #{pr}"), &why),
    };
    let (gh, dir, repo, head) = (
        ctx.gh.clone(),
        ctx.repo_dir(),
        project.github_repo.clone(),
        view.head_ref_oid.clone(),
    );
    let read = tokio::task::spawn_blocking(move || {
        let st = commit_checks(&gh, &dir, &repo, &head, &|_| {})?;
        let logs = if st.state == CommitState::Fail {
            ci_logs(&gh, &dir, &repo, &st.failed, &|_| {})
        } else {
            Vec::new()
        };
        anyhow::Ok((st, logs))
    })
    .await;
    let (checks, logs) = match read {
        Ok(Ok(read)) => read,
        Ok(Err(why)) => return stay_failed(done, run, flow, "Couldn't read its checks", &format!("{why:#}")),
        Err(why) => return stay_failed(done, run, flow, "Couldn't read its checks", &why.to_string()),
    };
    let since = *flow.ci_wait_since.get_or_insert(now);
    let mut state = checks.state;
    // With CI only when ready only the run the label started counts: a failure from before it is not this run's.
    if when_ready
        && state == CommitState::Fail
        && let (Some(label), Some(ended)) = (flow.ci_ready_at, ended_at(&checks.failed))
        && (ended as i64) < label
    {
        state = CommitState::Pending;
    }
    let wait_max = i64::from(t.ci_max_minutes) * 60_000;
    let grace = i64::from(t.ci_grace_minutes) * 60_000;
    match state {
        CommitState::Pass => {
            flow.ci_reruns = 0;
            let at_approved = flow.approved_sha.as_deref() == Some(view.head_ref_oid.as_str());
            run.approved_at_head = at_approved;
            if at_approved && flow.recheck {
                // Brought up to date where CI doesn't run the lane's checks: they run again on the combination too.
                flow.recheck = false;
                run.state = ItemState::Checking;
                done.notes.push("CI passed on the approved changes, brought up to date; running the checks here again before it merges".into());
            } else {
                run.ci_finished(CiOutcome::Passed, project);
                if at_approved {
                    flow.reviewed_sha = flow.approved_sha.clone();
                    done.notes.push("CI passed on the approved changes; merging".into());
                } else {
                    done.notes.push("CI passed".into());
                }
            }
            done.run = run;
            done.flow = Some(flow);
            done
        }
        CommitState::None => {
            if now - since > grace {
                let note = format!("No CI checks reported after {}", words(t.ci_grace_minutes));
                return carry_on(&ctx, done, run, flow, pr, note).await;
            }
            stay(done, run, flow)
        }
        CommitState::Pending => {
            if now - since > wait_max {
                // Stuck (a runner that never picked it up, a hung job): cancelled, so the next poll reads it as cancelled and
                // runs it again, within the same maxCiReruns.
                let (reruns, max) = (flow.ci_reruns + 1, t.max_ci_reruns);
                let host = ctx.ci_host();
                if reruns <= max
                    && tokio::task::spawn_blocking(move || host.cancel_pending(pr))
                        .await
                        .is_ok_and(|ok| ok)
                {
                    flow.ci_reruns = reruns;
                    flow.ci_wait_since = Some(now);
                    done.notes.push(format!(
                        "CI was pending for over {}; cancelled it, to run it again",
                        words(t.ci_max_minutes)
                    ));
                    return stay(done, run, flow);
                }
                run.escalate();
                done.notes
                    .push(format!("CI has been pending for over {}.", words(t.ci_max_minutes)));
                done.run = run;
                done.flow = Some(flow);
                return done;
            }
            stay(done, run, flow)
        }
        CommitState::Fail => {
            // Every red run was only stopped (a newer run cancelled it, or a person did), not failed: CI runs again,
            // keeping its place, with no lane.
            let cancelled = checks.failed.iter().all(|f| f.cancelled);
            let (reruns, max) = (flow.ci_reruns + 1, t.max_ci_reruns);
            if cancelled && reruns <= max {
                let host = ctx.ci_host();
                if tokio::task::spawn_blocking(move || host.rerun(pr, false))
                    .await
                    .is_ok_and(|ok| ok)
                {
                    flow.ci_reruns = reruns;
                    flow.ci_wait_since = Some(now);
                    if when_ready {
                        flow.ci_ready_at = Some(now);
                    }
                    done.notes.push(format!(
                        "CI's run was cancelled before it finished, not failed; running it again ({reruns}/{max})"
                    ));
                    return stay(done, run, flow);
                }
            }
            let names: Vec<&str> = checks.failed.iter().map(|f| f.name.as_str()).collect();
            run.ci_finished(CiOutcome::Failed, project);
            if run.state == ItemState::Implementing {
                done.send_back = Some(failed_note(&checks.failed, &logs, "pull request", Some(WHAT_MAX)));
                done.notes.push(format!("CI failed: {}", names.join(", ")));
            } else {
                done.notes.push(format!(
                    "CI still failing after {} rounds: {}",
                    project.max_check_rounds,
                    names.join(", ")
                ));
            }
            done.run = run;
            done.flow = Some(flow);
            done
        }
    }
}

/// No CI to wait for (none runs for the base, or none showed up): on to the lane's checks. With CI only when ready, a change
/// approved at its head has had them and its review, so it merges (else it would come back here).
async fn carry_on(
    ctx: &Ctx,
    mut done: StepDone,
    mut run: ItemRun,
    mut flow: FlowRecord,
    pr: u64,
    note: String,
) -> StepDone {
    run.approved_at_head = false;
    if ci_when_ready_active(&ctx.project)
        && let Some(approved) = flow.approved_sha.clone()
    {
        let view = match ctx.view(pr).await {
            Ok(view) => view,
            Err(why) => return stay_failed(done, run, flow, &format!("Couldn't read pull request #{pr}"), &why),
        };
        run.approved_at_head = view.head_ref_oid == approved;
        if run.approved_at_head {
            flow.reviewed_sha = Some(approved);
        }
    }
    run.ci_finished(CiOutcome::NoChecksRan, &ctx.project);
    done.notes.push(if run.state == ItemState::Merging {
        format!("{note}; merging the approved changes")
    } else {
        format!("{note}; carrying on")
    });
    flow.ci_note = Some(note);
    done.run = run;
    done.flow = Some(flow);
    done
}
