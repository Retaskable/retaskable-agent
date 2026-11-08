//! The draft step of a project's loop (docs/specs/agent/06.5-spec-drafts.md steps 1, 2 and 4): while the
//! project has no draft running and isn't paused, asks the store for the next draft (`draft_claim_next`),
//! makes its docs-only worktree, runs the drafter, and reports how it ended with `draft_report`. The
//! loop keeps only the running draft's id and session id, in memory: the store has the draft, and a
//! claim that comes back with a session id resumes that session, so a restart carries on. What a person
//! does with the draft afterwards is mostly not here: Edit is the `draft_edit` command (`drafts::actions`); Approve, Merge into main and Discard are carried out by `driver::land`. A claim that carries send-back notes or answers resumes the session with them, and answers are acknowledged with `draft_answer_taken`.
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::Shared;
use super::project::{ProjectLoop, signed_out};
use super::start_gate::StartGate;
use crate::ask::follow_up;
use crate::backlog::drafts::{ClaimedDraft, DraftReport, FixItem};
use crate::backlog::{BacklogError, ClaimNextAgent, Client};
use crate::config::ProjectConfig;
use crate::draft_worktree::{branch_name, slug};
use crate::drafts::approval::approves_itself;
use crate::drafts::model::{Question, checked_breakdown};
use crate::drafts::session::{Outcome, sort_fixes};
use crate::drafts::{DrafterResult, DrafterStatus};
use crate::engines::Role;
use crate::implementer::Interrupted;
use crate::implementer::publish::Published;
use crate::implementer::session::new_session_id;
use crate::view::DraftCommand;

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
    /// The `engine-placement` flag: Off, only a Claude drafter runs.
    pub placement: bool,
    /// With `engine-start-rules` On, the engine to start on when it isn't the drafter's chosen one.
    pub engine: Option<crate::engines::EngineId>,
    /// The sessions running now: this one is listed while it runs.
    pub sessions: crate::orchestrator::live::sessions::Sessions,
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
        lanes: u32,
    ) {
        if self.drafting() || *slots == 0 || shared.stopping {
            return;
        }
        let gate = StartGate::for_lanes(self.flags.on("engine-start-rules").await, self, lanes, shared);
        let engine = match &gate {
            Some(gate) => match gate.role_start(self, shared, Role::Drafter) {
                Some(engine) => engine,
                None => return,
            },
            None if signed_out(shared) => return,
            None => None,
        };
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
            flags: self.flags.clone(),
            repo,
            token,
            lease: claim.token,
            project: self.project.clone(),
            usage: self.store.usage(),
            host: host.clone(),
            approve: Approve {
                commands: shared.links.item_commands.clone(),
                project: self.id.clone(),
            },
            sessions: self.live.sessions().clone(),
            engine,
            retried: claim.draft.other.get("retried").and_then(Value::as_bool) == Some(true),
        };
        let id = claim.draft.id.clone();

        let sid = session_id.clone();
        tracing::info!("{}: drafting {id} (session {session_id})", self.id);
        let task = tokio::spawn(async move {
            if let Err(e) = work(ctx, claim.draft, sid, resume).await {
                tracing::warn!("a draft's report didn't reach the store: {e}");
            }
        });
        self.draft = Some(RunningDraft { id, task });
    }
}

/// Where a plan that approves itself queues its Approve, as if a person had pressed it.
struct Approve {
    commands: crate::view::ItemCommands,
    project: String,
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
    flags: std::sync::Arc<crate::flags::Flags>,
    repo: u64,
    token: String,
    lease: String,
    project: ProjectConfig,
    usage: Option<Value>,
    host: std::sync::Arc<dyn DraftHost>,
    /// The draft already went back to the queue once by itself.
    retried: bool,
    engine: Option<crate::engines::EngineId>,
    approve: Approve,
    sessions: crate::orchestrator::live::sessions::Sessions,
}

impl Ctx {
    async fn report(&self, id: &str, report: DraftReport) -> Result<(), BacklogError> {
        self.client
            .draft_report(self.repo, Some(&self.token), id, &self.lease, &report)
            .await
    }

    /// Retaskable's own message in the chat about `draft`: into the ask that wanted it, or a new conversation. The draft's
    /// state is already reported, so a message the store doesn't take is only logged.
    /// Only with the `ask-claude` flag On: Off is today's behaviour, no message.
    async fn say(&self, draft: &ClaimedDraft, text: &str) {
        if !self.flags.on("ask-claude").await {
            return;
        }
        let posted = (self.client)
            .ask_post(self.repo, Some(&self.token), text, &draft.id, draft.ask_id.as_deref())
            .await;
        if let Err(e) = posted {
            tracing::warn!("a draft's chat message didn't reach the store: {e}");
        }
    }

    /// A failed draft: a first failure goes back to the queue by itself, once (simple-flow.md section 6); one that
    /// failed on its retry waits for a person as `failed`.
    async fn failed(&self, id: &str, report: DraftReport) -> Result<(), BacklogError> {
        if self.retried {
            return self.needs_person(id, "failed", report).await;
        }
        let summary = report.summary.as_deref().unwrap_or("it failed");
        self.report(
            id,
            DraftReport {
                status: Some("queued".into()),
                summary: Some(format!("Trying again by itself: {summary}")),
                retried: Some(true),
                ..Default::default()
            },
        )
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
        Ok(Err(e)) => return ctx.failed(&id, failed(format!("{e:#}"))).await,
        Err(e) => return ctx.failed(&id, failed(e.to_string())).await,
    };
    // The session now has the person's answers: the store clears them, and keeps them off the next claim.
    if draft.answer.is_some() {
        ctx.client
            .draft_answer_taken(ctx.repo, Some(&ctx.token), &id, &ctx.lease)
            .await?;
    }
    let job_session = session_id.clone();
    let job = DraftJob {
        project: ctx.project.clone(),
        follow_up: follow_up(&draft),
        draft: draft.clone(),
        dir: dir.clone(),
        branch: branch.clone(),
        session_id,
        resume,
        usage: ctx.usage.clone(),
        placement: ctx.flags.on("engine-placement").await,
        engine: ctx.engine,
        sessions: ctx.sessions.clone(),
    };
    let result = match ctx.host.run(job).await {
        Ok(result) => result,
        // Cut by a restart or Quit: left as it is, to be claimed and resumed by its session id.
        Err(e) if e.downcast_ref::<Interrupted>().is_some() => return Ok(()),
        Err(e) => return ctx.failed(&id, failed(format!("{e:#}"))).await,
    };
    // A Cursor draft's chat id is the one the session reported, kept so a revision's `--resume` is that chat.
    if let Some(chat) = result.session_id.as_deref().filter(|c| *c != job_session) {
        ctx.report(
            &id,
            DraftReport {
                session_id: Some(chat.to_string()),
                ..Default::default()
            },
        )
        .await?;
    }
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
            .await?;
            ctx.say(&draft, follow_up::HAS_QUESTIONS).await;
            Ok(())
        }
        (None, DrafterStatus::Failed) => ctx.failed(&id, failed(result.summary.clone())).await,
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
            open_pr(&ctx, &id, &draft, &dir, &branch, &result).await
        }
    }
}

/// Pushes the branch, opens the PR and reports the draft as waiting for approval. A revision that
/// names no items keeps the earlier ones (`itemIds` left out).
async fn open_pr(
    ctx: &Ctx,
    id: &str,
    draft: &ClaimedDraft,
    dir: &Path,
    branch: &str,
    r: &DrafterResult,
) -> Result<(), BacklogError> {
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
                .failed(
                    id,
                    DraftReport {
                        summary: Some(why),
                        ..Default::default()
                    },
                )
                .await;
        }
        Err(e) => {
            return ctx
                .failed(
                    id,
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
    .await?;
    // A plan someone asked for goes ahead on that yes: the same Approve a person's press queues, which the land step
    // carries out on its next pass (paused or not) by the project's workflow.
    let by = draft.other.get("requestedBy").and_then(Value::as_str);
    let kind_release = draft.other.get("kind").and_then(Value::as_str) == Some("release");
    let trial = draft.intake.as_ref().is_some_and(|i| i.trial);
    let itself = approves_itself(ctx.project.plans, by, r.risk.as_deref(), kind_release, trial);
    if !itself {
        ctx.say(draft, follow_up::WRITTEN).await;
    }
    if itself {
        let Approve { commands, project } = &ctx.approve;
        commands.push_draft(DraftCommand::Approve {
            project: project.clone(),
            draft: id.to_string(),
        });
    }
    Ok(())
}
