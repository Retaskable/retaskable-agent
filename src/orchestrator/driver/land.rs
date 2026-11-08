//! **Approve and merge** and **Merge into main** on a spec draft (docs/specs/agent/06.5-spec-drafts.md step 3,
//! github-workflow.md section 9). The connector only queues the press ([`crate::view::DraftCommand`]); here, paused or
//! not, the draft is taken from the store and, when the project's workflow lets this computer merge, its pull
//! request is squash-merged with `gh`. Behind the base, it is rebased when that's clean and merged again, for a few
//! seconds while GitHub catches up. A merged draft's worktree goes, its items' headers reach the store at once
//! (`upsert_from_specs`, with the draft's items approved) and the store is told it's `merged`. When the pattern only
//! opens the pull request, Approve merges nothing (the draft waits for **Merge into main**), and any merge error
//! puts the draft back to waiting for approval with the reason in its report.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::task::JoinHandle;

use super::drafts::{DraftHost, DraftJob, claim_agent, rebase_request};
use super::project::signed_out;
use super::{ProjectLoop, Shared};
use crate::backlog::Client;
use crate::backlog::drafts::{DraftReport, TakenDraft};
use crate::config::ProjectConfig;
use crate::drafts::DrafterStatus;
use crate::drafts::land::{deletes_head, pr_number, workflow};
use crate::view::DraftCommand;

/// Times a conflicted draft goes back to its drafter to rebase before it waits for approval again.
const HAND_BACKS: u32 = 2;
/// Tries at the merge once the branch is rebased, and the wait between them.
const MERGE_TRIES: u32 = 5;
pub(super) const MERGE_WAIT: Duration = Duration::from_secs(2);

/// What a merged draft adds to the next `upsert_from_specs`.
pub(super) struct Landed {
    item_ids: Vec<String>,
    asked_by: Option<String>,
    draft_id: String,
}

/// The presses being carried out, and what merged drafts have yet to tell the store.
#[derive(Default)]
pub(super) struct Landing {
    tasks: Vec<JoinHandle<Option<Landed>>>,
    approved: Vec<String>,
    asked_by: BTreeMap<String, String>,
}

impl Landing {
    pub(super) fn approved(&self) -> Vec<String> {
        self.approved.clone()
    }

    pub(super) fn asked_by(&self) -> BTreeMap<String, String> {
        self.asked_by.clone()
    }

    /// The store took the headers: nothing is owed for those drafts.
    pub(super) fn sent(&mut self) {
        self.approved.clear();
        self.asked_by.clear();
    }
}

struct Ctx {
    client: Arc<Client>,
    repo: u64,
    token: String,
    lease: String,
    project: ProjectConfig,
    host: Arc<dyn DraftHost>,
    wait: Duration,
    placement: bool,
}

impl Ctx {
    async fn report(&self, id: &str, report: DraftReport) {
        let sent = (self.client).draft_report(self.repo, Some(&self.token), id, &self.lease, &report);
        if let Err(e) = sent.await {
            tracing::warn!("a draft's report didn't reach the store: {e}");
        }
    }

    /// Lets go of a draft taken for a press that can't be carried out.
    async fn release(&self, id: &str) {
        if let Err(e) = (self.client)
            .draft_release(self.repo, Some(&self.token), id, &self.lease)
            .await
        {
            tracing::warn!("couldn't let go of draft {id}: {e}");
        }
    }

    /// Back to waiting for approval, the reason on its timeline.
    async fn back(&self, id: &str, why: &str) {
        let report = DraftReport {
            status: Some("needs_person".into()),
            detail: Some("awaiting_approval".into()),
            summary: Some(format!("Couldn't merge: {why}")),
            ..Default::default()
        };
        self.report(id, report).await;
    }

    /// `gh pr merge`, up to `tries` times.
    async fn merge(&self, pr: u64, delete_branch: bool, tries: u32) -> Result<(), String> {
        let mut last = String::new();
        for i in 0..tries {
            if i > 0 {
                tokio::time::sleep(self.wait).await;
            }
            let (host, project) = (self.host.clone(), self.project.clone());
            let done = tokio::task::spawn_blocking(move || host.merge(&project, pr, delete_branch)).await;
            match done {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => last = format!("{e:#}"),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    /// Rebases the draft's branch onto the base when that's clean.
    async fn rebase(&self, id: &str, branch: &str) -> Result<bool, String> {
        let (host, project, id, branch) = (
            self.host.clone(),
            self.project.clone(),
            id.to_string(),
            branch.to_string(),
        );
        match tokio::task::spawn_blocking(move || host.rebase(&project, &id, &branch)).await {
            Ok(done) => done.map_err(|e| format!("{e:#}")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Resumes the draft's drafter session to rebase the branch and resolve the conflicts, then force-pushes what it did.
    async fn hand_back(&self, taken: &TakenDraft, branch: &str, reason: &str) -> Result<(), String> {
        let draft = &taken.draft;
        let Some(session_id) = draft.session_id.clone() else {
            return Err("the draft has no session to resume".into());
        };
        let (host, project, id, b) = (
            self.host.clone(),
            self.project.clone(),
            draft.id.clone(),
            branch.to_string(),
        );
        let dir = tokio::task::spawn_blocking(move || host.prepare(&project, &id, &b))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))?;
        let job = DraftJob {
            project: self.project.clone(),
            draft: draft.clone(),
            dir: dir.clone(),
            branch: branch.to_string(),
            session_id,
            resume: true,
            follow_up: rebase_request(reason),
            usage: None,
            placement: self.placement,
            lighthouse: false,
        };
        let result = self.host.run(job).await.map_err(|e| format!("{e:#}"))?;
        if result.status == DrafterStatus::Failed {
            return Err(result.summary);
        }
        let (host, project, id, b) = (
            self.host.clone(),
            self.project.clone(),
            draft.id.clone(),
            branch.to_string(),
        );
        tokio::task::spawn_blocking(move || host.push(&project, &id, &b))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))
    }
}

fn text<'a>(draft: &'a TakenDraft, key: &str) -> Option<&'a str> {
    draft.draft.other.get(key).and_then(Value::as_str)
}

/// **Discard**: the pull request is closed with its branch (a failure is ignored, as it may be closed already), the
/// worktree goes, the project's remote is fetched, and the store is told the draft is `discarded`. A draft that
/// isn't waiting for a person is let go instead.
async fn discard(ctx: Ctx, id: String, taken: TakenDraft) -> Option<Landed> {
    if text(&taken, "status") != Some("needs_person") {
        tracing::info!("dropped a Discard on {id}: it isn't waiting for a person");
        ctx.release(&id).await;
        return None;
    }
    let pr = taken.draft.pr_url.as_deref().and_then(pr_number);
    let (host, project, draft_id) = (ctx.host.clone(), ctx.project.clone(), id.clone());
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(pr) = pr
            && let Err(e) = host.close(&project, pr)
        {
            tracing::warn!("{draft_id}: closing the pull request: {e:#}");
        }
        host.remove(&project, &draft_id);
        if let Err(e) = host.fetch(&project) {
            tracing::warn!("{draft_id}: fetching after the discard: {e:#}");
        }
    })
    .await;
    let report = DraftReport {
        status: Some("discarded".into()),
        detail: Some("discarded".into()),
        ..Default::default()
    };
    ctx.report(&id, report).await;
    None
}

/// One press, from taking the draft to reporting it. `Some` when the pull request merged.
async fn land(ctx: Ctx, id: String, merge_press: bool, taken: TakenDraft) -> Option<Landed> {
    let wf = workflow(&ctx.project);
    let draft = &taken.draft;
    let pr = draft.pr_url.as_deref().and_then(pr_number);
    let waiting = matches!(
        (text(&taken, "status"), text(&taken, "detail")),
        (Some("needs_person"), Some("awaiting_approval" | "awaiting_merge"))
    );
    let (Some(pr), true) = (pr, waiting) else {
        tracing::info!("dropped a press on {id}: it isn't waiting for approval here");
        ctx.release(&id).await;
        return None;
    };
    let delete = wf.delete_branch && draft.branch.as_deref().is_some_and(deletes_head);
    if merge_press {
        let Some(into) = &wf.person_merges_into else {
            tracing::info!("dropped a Merge into main on {id}: this project's pattern doesn't have it");
            ctx.release(&id).await;
            return None;
        };
        if let Err(why) = ctx.merge(pr, delete, 1).await {
            ctx.back(&id, &format!("into {into}: {why}")).await;
            return None;
        }
    } else if !wf.we_merge {
        let into = wf.person_merges_into.as_deref().unwrap_or(&ctx.project.base_branch);
        let report = DraftReport {
            status: Some("needs_person".into()),
            detail: Some("awaiting_merge".into()),
            summary: Some(format!("Approved. Waiting for you to merge it into {into}.")),
            ..Default::default()
        };
        ctx.report(&id, report).await;
        return None;
    } else if let Err(first) = ctx.merge(pr, delete, 1).await {
        // Behind the base, perhaps: rebase it when that's clean, and merge once GitHub sees the push.
        let base = &ctx.project.base_branch;
        let Some(branch) = draft.branch.as_deref() else {
            ctx.back(&id, &first).await;
            return None;
        };
        let mut hand_backs = 0;
        let why = loop {
            match ctx.rebase(&id, branch).await {
                Ok(true) => break ctx.merge(pr, delete, MERGE_TRIES).await.err(),
                Ok(false) if hand_backs < HAND_BACKS => {
                    hand_backs += 1;
                    let reason = format!("it conflicts with {base}");
                    if let Err(e) = ctx.hand_back(&taken, branch, &reason).await {
                        break Some(format!("{reason}, and the drafter couldn't rebase it: {e}"));
                    }
                }
                Ok(false) => {
                    break Some(format!(
                        "it conflicts with {base}, even after going back to the drafter {HAND_BACKS} times. Send it back with notes."
                    ));
                }
                Err(e) => break Some(e),
            }
        };
        if let Some(why) = why {
            ctx.back(&id, &why).await;
            return None;
        }
    }
    let (host, project, draft_id) = (ctx.host.clone(), ctx.project.clone(), id.clone());
    let _ = tokio::task::spawn_blocking(move || {
        host.remove(&project, &draft_id);
        if let Err(e) = host.fetch(&project) {
            tracing::warn!("{draft_id}: fetching after the merge: {e:#}");
        }
    })
    .await;
    let report = DraftReport {
        status: Some("merged".into()),
        detail: Some("merged".into()),
        summary: Some(format!(
            "Merged {}",
            draft.pr_url.as_deref().unwrap_or("the pull request")
        )),
        ..Default::default()
    };
    ctx.report(&id, report).await;
    let item_ids = (draft.other.get("itemIds").and_then(Value::as_array))
        .map(|l| l.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Some(Landed {
        item_ids,
        asked_by: text(&taken, "requestedBy").map(String::from),
        draft_id: id,
    })
}

impl ProjectLoop {
    /// Takes in this project's queued presses and starts each; tells the store the headers of the drafts that merged
    /// since the last pass, paused or not.
    pub(super) async fn land_drafts(&mut self, host: &Arc<dyn DraftHost>, shared: &Shared<'_>, wait: Duration) {
        let mut merged = false;
        for task in std::mem::take(&mut self.landing.tasks) {
            if !task.is_finished() {
                self.landing.tasks.push(task);
            } else if let Ok(Some(landed)) = task.await {
                tracing::info!("{}: merged draft {}", self.id, landed.draft_id);
                for id in &landed.item_ids {
                    if let Some(by) = &landed.asked_by {
                        self.landing.asked_by.insert(id.clone(), by.clone());
                    }
                }
                self.landing.approved.extend(landed.item_ids);
                merged = true;
            }
        }
        if merged {
            // The merge is fetched already; the headers go now, not on the minute's fetch.
            self.report(shared.links).await;
        }
        let presses = shared.links.item_commands.take_drafts(&self.id);
        if presses.is_empty() || shared.stopping || signed_out(shared) {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let placement = self.flags.on("engine-placement").await;
        for press in presses {
            let (DraftCommand::Approve { draft, .. }
            | DraftCommand::Merge { draft, .. }
            | DraftCommand::Discard { draft, .. }) = &press;
            let id = draft.clone();
            let merge_press = matches!(press, DraftCommand::Merge { .. });
            let agent = claim_agent(shared.links, &self.project);
            let taken = match self.client.draft_take(repo, Some(&token), &id, &agent).await {
                Ok(taken) => taken,
                Err(e) => {
                    tracing::warn!("{}: couldn't take draft {id}: {e}", self.id);
                    continue;
                }
            };
            let ctx = Ctx {
                client: self.client.clone(),
                repo,
                token: token.clone(),
                lease: taken.token.clone(),
                project: self.project.clone(),
                host: host.clone(),
                wait,
                placement,
            };
            let task = if matches!(press, DraftCommand::Discard { .. }) {
                tokio::spawn(discard(ctx, id, taken))
            } else {
                tokio::spawn(land(ctx, id, merge_press, taken))
            };
            self.landing.tasks.push(task);
        }
    }
}
