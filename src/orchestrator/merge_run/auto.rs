//! The `auto_merge` and `queue` lands (docs/specs/pieces.md section 3.2): the change is up to date and mergeable, so GitHub is
//! asked to merge it when its checks pass (`gh pr merge --auto`), `merge_result handed_off` tells the store, and the run
//! stays open holding the turn (phase `auto_merge`), reading the change every `ask_every`, until it merged, closed or
//! conflicts. A repo with auto-merge turned off never says yes: once the change is mergeable it is merged here.
use super::*;
use crate::backlog::merge::MergeOutcome;
use crate::drafts::land::deletes_head;
use crate::runs::AUTO_MERGE;

/// What `view` reads while waiting: `gh pr view --json` fields.
const FIELDS: &str = "mergeable,mergeStateStatus,state,autoMergeRequest";
const CLOSED: &str = "The change was closed without merging.";

/// GitHub's refusal to turn auto-merge on: its words without gh's `GraphQL: ` prefix, at most 200 characters; none
/// when auto-merge is already on.
fn refusal(stderr: &str) -> Option<String> {
    let said = stderr.rsplit_once("GraphQL: ").map_or(stderr, |(_, rest)| rest);
    if said.to_lowercase().contains("already") {
        return None;
    }
    Some(said.trim().chars().take(200).collect())
}

/// Whether the refusal says the repository has auto-merge turned off (Settings, Allow auto-merge), which trying again
/// never fixes.
fn turned_off(reason: &str) -> bool {
    let r = reason.to_lowercase();
    r.contains("auto merge is not allowed")
        || r.contains("automerge is not allowed")
        || r.contains("enablepullrequestautomerge")
}

/// A merge refused for its method alone ("Merge commits are not allowed on this repository").
fn method_refused(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    ["merge commits", "squash merging", "rebase merging", "rebasing"]
        .iter()
        .any(|what| {
            ["are", "is"]
                .iter()
                .any(|verb| e.contains(&format!("{what} {verb} not allowed")))
        })
}

impl Job {
    /// Whether GitHub lands the change: the project's `land` is `auto_merge` or `queue`. A `queue` piece is handled
    /// exactly like `auto_merge` (same `gh pr merge --auto`, handed off, waited for holding the turn), unlike the
    /// TypeScript agent, which never marks it handed off, so its turn is given up as `sent_back` and nothing watches
    /// the change (Retaskable/Retaskable.com#3851).
    pub(super) fn auto(&self) -> bool {
        matches!(
            workflow(&self.request.context.config).land.as_str(),
            "auto_merge" | "queue"
        )
    }

    /// A run that starts again (`merge_open`) reads the change first: merged or closed ends the run, auto-merge already
    /// on goes on waiting; `Ok` when neither, and the change lands as usual. `Err` is how the run ends.
    pub(super) async fn reopened(&self, lock: &str, pr: &str, branch: &str, piece: &str) -> Result<(), Outcome> {
        let view = match self.gh_json::<View>(&["pr", "view", pr, "--json", FIELDS]).await {
            Ok(v) => v,
            Err(why) => return Err(failed(why)),
        };
        if self.stopped() {
            return Err(None);
        }
        match view.state.as_str() {
            "MERGED" => Err(self.merged(pr).await),
            "CLOSED" => Err(failed(CLOSED)),
            _ if view.auto_merge_request.is_some() => Err(self.wait(lock, pr, branch, piece, true).await),
            _ => Ok(()),
        }
    }

    /// Asks GitHub to merge the change when its checks pass, tells the store, and waits.
    pub(super) async fn hand_off(&self, lock: &str, pr: &str, branch: &str, piece: &str) -> Outcome {
        if self.stopped() {
            return None;
        }
        if !self.request.context.locks.held(lock) {
            return self.lost();
        }
        let refused = match self.enable(pr).await {
            Ok(r) => r,
            Err(why) => return failed(why),
        };
        if self.stopped() {
            return None;
        }
        if refused.is_none() && !self.handed_off(lock).await {
            return self.lost();
        }
        self.wait(lock, pr, branch, piece, refused.is_none()).await
    }

    /// `gh pr merge --auto` with the project's method: GitHub's refusal, or `None` once auto-merge is on.
    async fn enable(&self, pr: &str) -> Result<Option<String>, String> {
        let flow = workflow(&self.request.context.config);
        let method = if self.sync { "--merge" } else { flow.method_flag };
        let mut args = vec!["pr", "merge", pr, method];
        if flow.delete_branch {
            args.push("--delete-branch");
        }
        args.push("--auto");
        let (_, err, ok) = self.gh(&args).await?;
        Ok(if ok { None } else { refusal(&err) })
    }

    /// `merge_result handed_off`: the turn stays held and the beats say `auto_merge`. False when the store says the
    /// turn is no longer ours or can't be asked (fail closed: the run ends, and the next one reads the change first).
    async fn handed_off(&self, lock: &str) -> bool {
        let ctx = &self.request.context;
        ctx.locks.set_phase(lock, AUTO_MERGE);
        match ctx
            .client
            .merge_result(ctx.repo, Some(&ctx.token), lock, &MergeOutcome::HandedOff)
            .await
        {
            Ok(answer) => answer.held && !self.stopped(),
            Err(e) => {
                tracing::warn!(turn = %lock, "the hand-off wasn't taken by the store: {e}");
                false
            }
        }
    }

    /// Each `ask_every` the change is read: merged ends `merged`, closed needs a person, a conflict is brought up to
    /// date; where auto-merge isn't on (yet, or any more) it is turned on again every `retry_after`, or, on a repo with it turned off
    /// and a change GitHub calls mergeable, merged here. The turn is lost when the loop says so (a stop, the ceiling).
    pub(super) async fn wait(&self, lock: &str, pr: &str, branch: &str, piece: &str, mut on: bool) -> Outcome {
        let ctx = &self.request.context;
        if on {
            ctx.locks.set_phase(lock, AUTO_MERGE);
        }
        loop {
            tokio::time::sleep(if on { self.delays.ask_every } else { self.retry_after() }).await;
            if self.stopped() {
                return None;
            }
            if !ctx.locks.held(lock) {
                return self.lost();
            }
            let view = match self.gh_json::<View>(&["pr", "view", pr, "--json", FIELDS]).await {
                Ok(v) => v,
                Err(why) => return failed(why),
            };
            if self.stopped() {
                return None;
            }
            match view.state.as_str() {
                "MERGED" => return self.merged(pr).await,
                "CLOSED" => return failed(CLOSED),
                _ => {}
            }
            if view.mergeable == "CONFLICTING" {
                return if self.may_update() {
                    self.update(lock, branch, piece).await
                } else {
                    self.out_of_rounds()
                };
            }
            if on {
                if view.auto_merge_request.is_some() {
                    continue;
                }
                // Turned off, or the change left the queue, since it was handed over (the TypeScript agent waits for
                // ever): it is turned on again, and the store told again.
                tracing::info!(turn = %lock, "auto-merge is no longer on for the change; turning it on again");
                on = false;
            }
            match self.enable(pr).await {
                Err(why) => return failed(why),
                Ok(None) => {
                    if self.stopped() {
                        return None;
                    }
                    if !self.handed_off(lock).await {
                        return self.lost();
                    }
                    on = true;
                }
                Ok(Some(reason)) if turned_off(&reason) && view.mergeable == "MERGEABLE" => {
                    match self.merge_approved(pr, branch).await {
                        Ok(()) => return self.merged(pr).await,
                        Err(why) => tracing::info!(turn = %lock, "couldn't merge yet: {why}"),
                    }
                }
                Ok(Some(reason)) => tracing::info!(turn = %lock, "GitHub didn't turn on auto-merge: {reason}"),
            }
            if self.stopped() {
                return None;
            }
        }
    }

    /// Merges the change itself, for a repository with auto-merge turned off: the project's method, then squash, rebase
    /// and merge in turn when the repo doesn't allow one. The head is deleted only when it is Retaskable's own branch.
    /// GitHub still refuses while a required check or review is missing, so this lands it no sooner than auto-merge would.
    async fn merge_approved(&self, pr: &str, branch: &str) -> Result<(), String> {
        let first = if self.sync {
            "--merge"
        } else {
            workflow(&self.request.context.config).method_flag
        };
        let mut methods = vec![first];
        for m in ["--squash", "--rebase", "--merge"] {
            if !methods.contains(&m) {
                methods.push(m);
            }
        }
        let delete = deletes_head(branch);
        for (i, method) in methods.iter().enumerate() {
            // No `--delete-branch`: it also deletes the local branch, which fails while a lane has it checked out.
            let args = vec!["pr", "merge", pr, *method];
            let (_, err, ok) = self.gh(&args).await?;
            // A merge that errored is read again: a change that shows merged has landed.
            if ok || self.shows_merged(pr, &err).await {
                if delete {
                    self.drop_branch(branch).await;
                }
                return Ok(());
            }
            if i + 1 == methods.len() || !method_refused(&err) {
                return Err(err.trim().to_string());
            }
        }
        unreachable!("the last method returns")
    }
}
