//! The draft step of a project's loop (docs/specs/agent/06.5-spec-drafts.md steps 1, 2 and 4): while the
//! project has no draft running and isn't paused, asks the store for the next draft (`draft_claim_next`),
//! makes its docs-only worktree, runs the drafter, and reports how it ended with `draft_report`. The
//! loop keeps only the running draft's id and session id, in memory: the store has the draft, and a
//! claim that comes back with a session id resumes that session, so a restart carries on. What a person
//! does with the draft afterwards is mostly not here: edit and discard are `drafts::actions` (commands), approve is later. A claim that carries send-back notes or answers resumes the session with them, and answers are acknowledged with `draft_answer_taken`.
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::Shared;
use super::project::{ProjectLoop, signed_out};
use crate::backlog::drafts::{ClaimedDraft, DraftReport, FixItem};
use crate::backlog::{BacklogError, ClaimNextAgent, Client};
use crate::config::ProjectConfig;
use crate::draft_worktree::{branch_name, slug};
use crate::drafts::model::{Question, checked_breakdown};
use crate::drafts::session::{Outcome, sort_fixes};
use crate::drafts::{DrafterResult, DrafterStatus};
use crate::implementer::Interrupted;
use crate::implementer::publish::Published;
use crate::implementer::session::new_session_id;

/// What a drafter session needs.
pub struct DraftJob {
    pub project: ProjectConfig,
    pub draft: ClaimedDraft,
    /// The draft's worktree.
    pub dir: PathBuf,
    pub branch: String,
    /// The id the session runs under, or the saved one to resume.
    pub session_id: String,
    /// Resume `session_id` with `follow_up` rather than start it.
    pub resume: bool,
    pub follow_up: String,
    /// The project's latest `state.usage` reading, for the drafter's model tier.
    pub usage: Option<Value>,
}

/// The parts of a draft that touch the machine: git, the code host and the engine. Faked in tests.
pub trait DraftHost: Send + Sync {
    /// Makes (or reuses) the draft's worktree on `branch`.
    fn prepare(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<PathBuf>;
    /// Runs the drafter. An error that downcasts to [`Interrupted`] is a restart or Quit cutting it.
    fn run(&self, job: DraftJob) -> BoxFuture<'static, Result<DrafterResult>>;
    /// Pushes the branch and opens the PR (or finds the open one).
    fn publish(&self, project: &ProjectConfig, dir: &Path, branch: &str, title: &str, body: &str) -> Result<Published>;
    /// Removes the worktree; the branch stays for the PR.
    fn remove(&self, project: &ProjectConfig, id: &str);
    /// Squash-merges (or as the project's merge method says) the pull request with `gh pr merge`.
    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool) -> Result<()>;
    /// Closes the pull request and deletes its branch, with `gh pr close --delete-branch`.
    fn close(&self, project: &ProjectConfig, pr: u64) -> Result<()>;
    /// Rebases the draft's pushed branch onto the base branch in its worktree and force-pushes with lease. False
    /// when it conflicts.
    fn rebase(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<bool>;
    /// Force-pushes with lease the branch the drafter rebased in the draft's worktree.
    fn push(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<()>;
    /// Fetches the project's remote, so the merge is seen.
    fn fetch(&self, project: &ProjectConfig) -> Result<()>;
}

/// The draft this project is drafting: only ids, never the draft itself.
pub(super) struct RunningDraft {
    pub id: String,
    /// The session that is drafting; a restart gets it back from the store's claim.
    #[allow(dead_code)]
    pub session_id: String,
    task: JoinHandle<()>,
}

const INTERRUPTED: &str = "You were interrupted; continue.";

/// What the resumed session is told: the person's answers, their send-back notes, a rebase request, or
/// that it was interrupted.
fn follow_up(d: &ClaimedDraft) -> String {
    if let Some(a) = &d.answer {
        format!(
            "The person answered your questions, in order:\n{}",
            a.answers.join("\n")
        )
    } else if let Some(n) = d.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        format!("The person sent the draft back with these notes:\n{n}")
    } else if let Some(r) = &d.rebase_reason {
        rebase_request(r)
    } else {
        INTERRUPTED.to_string()
    }
}

/// The follow-up that hands a conflicted branch back to the drafter.
pub(super) fn rebase_request(reason: &str) -> String {
    format!("Rebase the branch onto the base branch and resolve the conflicts, keeping both sides: {reason}")
}

impl ProjectLoop {
    /// Whether a draft is running here now; a finished one is forgotten.
    pub(super) fn drafting(&mut self) -> bool {
        if self.draft.as_ref().is_some_and(|d| d.task.is_finished()) {
            self.draft = None;
        }
        self.draft.is_some()
    }

    /// Claims and starts the next draft when none runs here and `slots` has room (a draft jumps the
    /// queue but counts toward `concurrency`). Never while paused or stopping: the caller doesn't ask.
    pub(super) async fn draft_pass(
        &mut self,
        slots: &mut u32,
        host: &std::sync::Arc<dyn DraftHost>,
        shared: &Shared<'_>,
    ) {
        if self.drafting() || *slots == 0 || shared.stopping || signed_out(shared) {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let agent = claim_agent(shared.links, &self.project);
        let claim = match self.client.draft_claim_next(repo, Some(&token), &agent, &[]).await {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(e) => return self.store_error("draft_claim_next", &e),
        };
        *slots -= 1;
        let resume = claim.draft.session_id.is_some();
        let session_id = claim.draft.session_id.clone().unwrap_or_else(new_session_id);
        let ctx = Ctx {
            client: self.client.clone(),
            repo,
            token,
            lease: claim.token,
            project: self.project.clone(),
            usage: self.store.usage(),
            host: host.clone(),
        };
        let id = claim.draft.id.clone();
        let sid = session_id.clone();
        tracing::info!("{}: drafting {id} (session {session_id})", self.id);
        let task = tokio::spawn(async move {
            if let Err(e) = work(ctx, claim.draft, sid, resume).await {
                tracing::warn!("a draft's report didn't reach the store: {e}");
            }
        });
        self.draft = Some(RunningDraft { id, session_id, task });
    }
}

/// Who this computer is to the store, for taking or claiming a draft.
pub(super) fn claim_agent(links: &super::Links, project: &ProjectConfig) -> ClaimNextAgent {
    ClaimNextAgent {
        machine: links.machine.clone(),
        environment: links.environment.clone(),
        base_branch: project.base_branch.clone(),
        tags: links.tags.clone(),
    }
}

struct Ctx {
    client: std::sync::Arc<Client>,
    repo: u64,
    token: String,
    lease: String,
    project: ProjectConfig,
    usage: Option<Value>,
    host: std::sync::Arc<dyn DraftHost>,
}

impl Ctx {
    async fn report(&self, id: &str, report: DraftReport) -> Result<(), BacklogError> {
        self.client
            .draft_report(self.repo, Some(&self.token), id, &self.lease, &report)
            .await
    }

    /// An end that needs a person: waiting on them (`needs_person`) with the holder's exact state.
    async fn needs_person(&self, id: &str, detail: &str, report: DraftReport) -> Result<(), BacklogError> {
        self.report(
            id,
            DraftReport {
                status: Some("needs_person".into()),
                detail: Some(detail.into()),
                ..report
            },
        )
        .await
    }
}

/// One draft from claim to report. An `Err` is a report the store didn't take (fail closed: the draft
/// stays leased to this computer, and nothing else is claimed until it ends).
async fn work(ctx: Ctx, draft: ClaimedDraft, session_id: String, resume: bool) -> Result<(), BacklogError> {
    let id = draft.id.clone();
    let branch = draft.branch.clone().unwrap_or_else(|| {
        let from = draft
            .titles
            .first()
            .or(draft.request.as_ref())
            .map_or("", String::as_str);
        branch_name(&id, &slug(from))
    });
    ctx.report(
        &id,
        DraftReport {
            status: Some("drafting".into()),
            session_id: Some(session_id.clone()),
            branch: Some(branch.clone()),
            ..Default::default()
        },
    )
    .await?;
    let failed = |why: String| DraftReport {
        summary: Some(why),
        ..Default::default()
    };
    let prepared = {
        let (host, project, id, branch) = (ctx.host.clone(), ctx.project.clone(), id.clone(), branch.clone());
        tokio::task::spawn_blocking(move || host.prepare(&project, &id, &branch)).await
    };
    let dir = match prepared {
        Ok(Ok(dir)) => dir,
        Ok(Err(e)) => return ctx.needs_person(&id, "failed", failed(format!("{e:#}"))).await,
        Err(e) => return ctx.needs_person(&id, "failed", failed(e.to_string())).await,
    };
    // The session now has the person's answers: the store clears them, and keeps them off the next claim.
    if draft.answer.is_some() {
        ctx.client
            .draft_answer_taken(ctx.repo, Some(&ctx.token), &id, &ctx.lease)
            .await?;
    }
    let job = DraftJob {
        project: ctx.project.clone(),
        follow_up: follow_up(&draft),
        draft: draft.clone(),
        dir: dir.clone(),
        branch: branch.clone(),
        session_id,
        resume,
        usage: ctx.usage.clone(),
    };
    let result = match ctx.host.run(job).await {
        Ok(result) => result,
        // Cut by a restart or Quit: left as it is, to be claimed and resumed by its session id.
        Err(e) if e.downcast_ref::<Interrupted>().is_some() => return Ok(()),
        Err(e) => return ctx.needs_person(&id, "failed", failed(format!("{e:#}"))).await,
    };
    let sorted = sort_fixes(&result);
    let ask = |qs: &[Question]| -> Vec<String> {
        match qs {
            [] => vec![result.summary.clone()],
            qs => qs.iter().map(|q| q.question.clone()).collect(),
        }
    };
    let questions = match (&sorted.outcome, &result.status) {
        (Outcome::Ask(qs), _) => Some(ask(qs)),
        (_, DrafterStatus::NeedsClarification) => Some(ask(&result.questions)),
        _ => None,
    };
    match (questions, &result.status) {
        (Some(questions), _) => {
            ctx.needs_person(
                &id,
                "needs_clarification",
                DraftReport {
                    summary: Some(result.summary.clone()),
                    questions: Some(questions),
                    ..Default::default()
                },
            )
            .await
        }
        (None, DrafterStatus::Failed) => ctx.needs_person(&id, "failed", failed(result.summary.clone())).await,
        (None, _) => {
            let outcome = sorted.outcome;
            for fix in &sorted.fix_items {
                let item = FixItem::new(&fix.id, &fix.title, &fix.remaining);
                let item = FixItem {
                    spec_refs: fix.spec_refs.clone(),
                    ..item
                };
                if let Err(e) = ctx.client.add_fix(ctx.repo, Some(&ctx.token), &item).await
                    && e.code() != Some("conflict")
                {
                    return Err(e);
                }
            }
            if outcome == Outcome::QueuedFixes {
                ctx.host.remove(&ctx.project, &id);
                return ctx
                    .report(
                        &id,
                        DraftReport {
                            status: Some("merged".into()),
                            detail: Some("merged".into()),
                            summary: Some(result.summary.clone()),
                            ..Default::default()
                        },
                    )
                    .await;
            }
            open_pr(&ctx, &id, &dir, &branch, &result).await
        }
    }
}

/// Pushes the branch, opens the PR and reports the draft as waiting for approval. A revision that
/// names no items keeps the earlier ones (`itemIds` left out).
async fn open_pr(ctx: &Ctx, id: &str, dir: &Path, branch: &str, r: &DrafterResult) -> Result<(), BacklogError> {
    let title = r
        .change_title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Spec: {}", r.title.clone().unwrap_or_else(|| id.to_string())));
    let body = r
        .change_body
        .clone()
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| r.summary.clone());
    let published = {
        let (host, project, dir, branch) = (
            ctx.host.clone(),
            ctx.project.clone(),
            dir.to_path_buf(),
            branch.to_string(),
        );
        let title = title.clone();
        tokio::task::spawn_blocking(move || host.publish(&project, &dir, &branch, &title, &body)).await
    };
    let published = match published {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            let why = format!("The spec is committed but couldn't be published: {e:#}");
            return ctx
                .needs_person(
                    id,
                    "failed",
                    DraftReport {
                        summary: Some(why),
                        ..Default::default()
                    },
                )
                .await;
        }
        Err(e) => {
            return ctx
                .needs_person(
                    id,
                    "failed",
                    DraftReport {
                        summary: Some(e.to_string()),
                        ..Default::default()
                    },
                )
                .await;
        }
    };
    ctx.host.remove(&ctx.project, id);
    let titles = if r.pieces.is_empty() {
        vec![title]
    } else {
        r.pieces.iter().map(|p| p.title.clone()).collect()
    };
    let ids = r.entry_ids();
    let breakdown = checked_breakdown(r.breakdown.as_ref(), &ids).map(|b| {
        json!({
            "build": b.build.iter().map(|p| json!({"id": p.id, "line": p.line, "size": p.size})).collect::<Vec<_>>(),
            "people": b.people.iter().map(|p| json!({"step": p.step, "who": p.who, "waitOn": p.wait_on})).collect::<Vec<_>>(),
        })
    });
    ctx.needs_person(
        id,
        "awaiting_approval",
        DraftReport {
            titles: Some(titles),
            summary: Some(r.summary.clone()),
            pr_url: Some(published.url),
            item_ids: (!ids.is_empty()).then_some(ids),
            breakdown,
            ..Default::default()
        },
    )
    .await
}
