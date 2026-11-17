//! The draft step of a project's loop (docs/specs/agent/06.5-spec-drafts.md steps 1, 2 and 4): while the
//! project has no draft running and isn't paused, asks the store for the next draft (`draft_claim_next`),
//! makes its docs-only worktree, runs the drafter, and reports how it ended with `draft_report`. The
//! loop keeps only the running draft's id and session id, in memory: the store has the draft, and a
//! claim that comes back with a session id resumes that session, so a restart carries on. What a person
//! does with the draft afterwards is mostly not here: Edit is the `draft_edit` command (`drafts::actions`); Approve, Merge into main and Discard are carried out by `driver::land`. A claim that carries send-back notes or answers resumes the session with them, and answers are acknowledged with `draft_answer_taken`.
use crate::text::is_blank;
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::Shared;
use super::draft_owed::{Kept, Sender};
use super::project::ProjectLoop;
use super::signed_out::signed_out;
use super::start_gate::StartGate;
use crate::ask::classify::Classified;
use crate::backlog::drafts::{ClaimedDraft, DraftReport, FixItem, PlanDecision, PlanFacts, Remaining};
use crate::backlog::{BacklogError, ClaimNextAgent, Client};
use crate::config::ProjectConfig;
use crate::design_board::needs_design::item_kinds;
use crate::design_board::needs_design::{preview_files, preview_problem};
use crate::design_board::session::Idea;
use crate::design_board::{Drawing, Drawn, Keeping, draw_design_board_with, keep_review};
use crate::draft_worktree::{branch_name, slug};
use crate::drafts::failure::{NO_REASON, RETRY_PREFIX, error_reason, reason_or};
use crate::drafts::model::{Question, checked_breakdown};
use crate::drafts::revision::{Revised, revises_of, revision_text, to_revision};
use crate::drafts::session::{Outcome, sort_fixes};
use crate::drafts::{DrafterResult, DrafterStatus};
use crate::engines::Role;
use crate::ideas::Ideas;
use crate::ideas::bug_sort::sort;
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
    /// The engine to start on when it isn't the drafter's chosen one.
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
    /// `gh pr merge`, as the project's merge method says, or `--squash` when `squash` (a person's Merge into main).
    fn merge(&self, project: &ProjectConfig, pr: u64, delete_branch: bool, squash: bool) -> Result<()>;
    /// Closes the pull request and deletes its branch, with `gh pr close --delete-branch`.
    fn close(&self, project: &ProjectConfig, pr: u64) -> Result<()>;
    /// Rebases the draft's pushed branch onto the base branch in its worktree and force-pushes with lease. False
    /// when it conflicts.
    fn rebase(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<bool>;
    /// Force-pushes with lease the branch the drafter rebased in the draft's worktree.
    fn push(&self, project: &ProjectConfig, id: &str, branch: &str) -> Result<()>;
    /// Where the pull request stands on the code host (`gh pr view --json state`), for a plan a person merged or closed
    /// there while it waited for approval. A host that can't tell says so, and the plan stays as it is.
    fn pr_state(&self, _project: &ProjectConfig, _pr: u64) -> Result<crate::drafts::land::PrState> {
        anyhow::bail!("this host can't read a pull request's state")
    }
    /// Fetches the project's remote, so the merge is seen.
    fn fetch(&self, project: &ProjectConfig) -> Result<()>;
    /// The files the draft changed, read in its worktree before it goes (to see whether the plan needs a design review,
    /// `design_board::needs_design::preview_files`). A host that can't read them says none.
    fn design_files(&self, _project: &ProjectConfig, _dir: &Path) -> Result<Vec<crate::drafts::files::DraftFile>> {
        Ok(Vec::new())
    }
    /// Asks the layout session (`design_board::session`) for the designs of a plan: its prompt in, its structured answer
    /// out, or the reason there is none.
    fn design_layout(
        &self,
        _project: &ProjectConfig,
        _prompt: String,
    ) -> BoxFuture<'static, Result<Value, crate::design_board::DesignError>> {
        Box::pin(async {
            Err(crate::design_board::DesignError::conflict(
                "this host can't draw designs",
            ))
        })
    }
    /// Asks the same kind of session for one plain sentence again (`design_board::keep`): its prompt in, its structured
    /// answer out, or the reason there is none.
    fn design_sentence(
        &self,
        _project: &ProjectConfig,
        _prompt: String,
    ) -> BoxFuture<'static, Result<Value, crate::design_board::DesignError>> {
        Box::pin(async {
            Err(crate::design_board::DesignError::conflict(
                "this host can't draw designs",
            ))
        })
    }
    /// How the idea's board socket is opened.
    fn board_transport(&self) -> std::sync::Arc<dyn crate::backlog_socket::Transport> {
        std::sync::Arc::new(crate::backlog_socket::Wire)
    }
    /// Sorts a person's idea with the classifier (one Haiku call). Anything that goes wrong is `Unavailable`.
    fn classify(&self, _project: &ProjectConfig, _text: String) -> BoxFuture<'static, Classified> {
        Box::pin(async { Classified::Unavailable })
    }
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
    } else if let Some(n) = d.notes.as_deref().filter(|n| !is_blank(n)) {
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
        self.drain_failures();
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
        // A final report kept from before a start goes first: a draft that moved is not taken again meanwhile.
        if self.drafting() || *slots == 0 || shared.stopping || self.final_reports_owed() {
            return;
        }
        let gate = StartGate::for_lanes(self, lanes, shared);
        let Some(engine) = gate.role_start(self, shared, Role::Drafter, true) else {
            return;
        };
        let Some(repo) = self.repo else { return };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let mut agent = claim_agent(shared.links, &self.project);
        agent.week = self.week_use();
        let claim = match self.client.draft_claim_next(repo, Some(&token), &agent, &[]).await {
            Ok(Some(claim)) => claim,
            Ok(None) => return,
            Err(e) => return self.store_error("draft_claim_next", &e),
        };
        let mut claim = claim;
        let resume = claim.draft.session_id.is_some();
        // A person's idea that is really a bug is moved to Bugs and planned as a fix; never blocks the draft.
        if !resume && !signed_out(shared) {
            let ideas = Ideas {
                client: &self.client,
                repo_id: repo,
                token: Some(&token),
            };
            let project = self.project.clone();
            let host = host.clone();
            sort(&ideas, &mut claim.draft, |text| host.classify(&project, text)).await;
        }
        // A resume keeps the engine its session ran on: it waits while that one is out, whatever else may start.
        let engine = if resume { None } else { engine };
        if resume && gate.role_start(self, shared, Role::Drafter, false).is_none() {
            let released = self
                .client
                .draft_release(repo, Some(&token), &claim.draft.id, &claim.token);
            if let Err(e) = released.await {
                tracing::warn!("{}: couldn't let go of draft {}: {e}", self.id, claim.draft.id);
                self.store_failure("draft_release", &claim.draft.id, &e).await;
            }
            return;
        }
        *slots -= 1;
        let session_id = claim.draft.session_id.clone().unwrap_or_else(new_session_id);
        let ctx = Ctx {
            client: self.client.clone(),
            repo,
            token,
            lease: claim.token,
            failures: self.failures.clone(),
            kept: self.drafts_owed.clone(),
            project: self.project.clone(),
            usage: self.store.usage(),
            host: host.clone(),
            approve: Approve {
                commands: shared.links.item_commands.clone(),
                project: self.id.clone(),
            },
            sessions: self.live.sessions().clone(),
            facts: crate::kept_facts::shared(self.store.dir()),
            engine,
            retried: claim.draft.other.get("retried").and_then(Value::as_bool) == Some(true),
            claude_signed_out: signed_out(shared),
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
        tags: links.tags.get(),
        week: None,
    }
}

impl ProjectLoop {
    /// This computer's Claude week as the store takes it, from the project's last usage readings
    /// (backlog/06.6 section 6.6.5); `None` when there's no reading or the week has reset.
    pub(super) fn week_use(&self) -> Option<crate::backlog::week::WeekUse> {
        let readings = [self.store.usage(), self.store.week_usage()];
        crate::backlog::week::WeekUse::of(readings.iter().flatten(), crate::telemetry::now_secs())
    }
}

/// The wait between sends of a final report the store didn't take.
const FINAL_RETRY: std::time::Duration = super::draft_owed::RETRY;

struct Ctx {
    client: std::sync::Arc<Client>,
    repo: u64,
    token: String,
    lease: String,
    failures: crate::backlog::failures::Failures,
    kept: Kept,
    project: ProjectConfig,
    usage: Option<Value>,
    host: std::sync::Arc<dyn DraftHost>,
    /// The draft already went back to the queue once by itself.
    retried: bool,
    engine: Option<crate::engines::EngineId>,
    approve: Approve,
    sessions: crate::orchestrator::live::sessions::Sessions,
    /// Claude was signed out when the draft started: the designs aren't asked of it.
    claude_signed_out: bool,
    /// The facts this project keeps for the site: the plan's files, once it waits for approval.
    facts: crate::kept_facts::KeptFacts,
}

impl Ctx {
    /// [`Self::report`] of a plan ready for approval, with the facts the store decides from and its answer: whether the plan goes
    /// ahead and whether it waits for a design review. `None` when the store gave none, and the plan then waits for a person.
    async fn report_plan(&self, id: &str, report: DraftReport) -> Result<Option<PlanDecision>, BacklogError> {
        let sent = (self.client)
            .draft_report_decided(self.repo, Some(&self.token), id, &self.lease, &report)
            .await;
        if let Err(e) = &sent {
            self.failures.add("draft_report", id, e);
        }
        sent
    }

    async fn report(&self, id: &str, report: DraftReport) -> Result<(), BacklogError> {
        let sent = (self.client)
            .draft_report(self.repo, Some(&self.token), id, &self.lease, &report)
            .await;
        if let Err(e) = &sent {
            self.failures.add("draft_report", id, e);
        }
        sent
    }

    /// The `merged` report of a plan whose items were all queued as fixes: the work is done, so it is kept in the
    /// project's folder and sent again every [`FINAL_RETRY`] while the store can't be reached or answers 5xx, also
    /// after a restart, for up to a day (the owner's outage rule, 2026-11-16: "it should be able to finish what it's
    /// doing but it can't pick up the next thing till the store is back."). A refusal that names a code (the draft
    /// moved on) ends it (`draft_owed.rs`). The TS agent sends once.
    async fn report_final(&self, id: &str, report: DraftReport) -> Result<(), BacklogError> {
        let sender = Sender {
            client: &self.client,
            repo: self.repo,
            token: &self.token,
            failures: &self.failures,
            kept: &self.kept,
            wait: FINAL_RETRY,
        };
        sender.send(id, &self.lease, &report, None).await
    }

    /// A failed draft: a first failure goes back to the queue by itself, once (simple-flow.md section 6); one that
    /// failed on its retry waits for a person as `failed`.
    async fn failed(&self, id: &str, report: DraftReport) -> Result<(), BacklogError> {
        let report = DraftReport {
            summary: Some(reason_or(report.summary.as_deref().unwrap_or(""), NO_REASON)),
            ..report
        };
        let summary = report.summary.clone().unwrap_or_default();
        if self.retried {
            return self.needs_person(id, "failed", report).await;
        }
        self.report(
            id,
            DraftReport {
                status: Some("queued".into()),
                summary: Some(format!("{RETRY_PREFIX}{summary}")),
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

    /// [`Self::needs_person`] for a plan ready for approval, answering the store's decision on its facts.
    async fn plan_ready(&self, id: &str, report: DraftReport) -> Result<Option<PlanDecision>, BacklogError> {
        self.report_plan(
            id,
            DraftReport {
                status: Some("needs_person".into()),
                detail: Some("awaiting_approval".into()),
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
        summary: Some(error_reason(&why, "error")),
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
        let taken = (ctx.client)
            .draft_answer_taken(ctx.repo, Some(&ctx.token), &id, &ctx.lease)
            .await;
        if let Err(e) = &taken {
            ctx.failures.add("draft_answer_taken", &id, e);
        }
        taken?;
    }
    // A fresh plan that revises an item is told about it. A store that can't be asked fails the plan: it never goes
    // ahead blind to its item (backlog.md section 8.1).
    let mut draft = draft;
    if !resume && let Some(item) = revises_of(&draft.other) {
        match ctx.client.item(ctx.repo, Some(&ctx.token), &item).await {
            Ok(found) => draft.revising = found.map(|f| revision_text(&f, &id)),
            Err(e) => {
                let why = format!("couldn't read item {item}, which this plan revises: {e}");
                return ctx.failed(&id, failed(why)).await;
            }
        }
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
            Ok(())
        }
        (None, DrafterStatus::Failed) => {
            let why = reason_or(&result.summary, "The drafter stopped without giving a reason.");
            ctx.failed(&id, failed(why)).await
        }
        (None, _) => {
            let outcome = sorted.outcome;
            // A fix asked on a card reopens the card's item when it can, once (card-revisions.md section 3.7).
            let revises = revises_of(&draft.other);
            let mut reopened = false;
            for fix in &sorted.fix_items {
                if let (Some(item), false) = (&revises, reopened) {
                    let by = draft.other.get("requestedBy").and_then(Value::as_str);
                    match (ctx.client)
                        .reopen_for_fix(ctx.repo, Some(&ctx.token), item, &fix.remaining, by)
                        .await
                    {
                        Ok(true) => {
                            reopened = true;
                            continue;
                        }
                        Ok(false) => {}
                        // A store that can't be reached: the fix isn't added as an item of its own instead.
                        Err(e) if e.code().is_none() => return ctx.failed(&id, failed(format!("{e}"))).await,
                        Err(_) => {}
                    }
                }
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
                    .report_final(
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

/// Lists the draft's changed files for the site. Files git can't list are not kept (the site then asks a computer).
async fn keep_files(ctx: &Ctx, id: &str, dir: &Path) {
    let (dir, base) = (dir.to_path_buf(), ctx.project.base_branch.clone());
    match tokio::task::spawn_blocking(move || crate::drafts::files::draft_files(&dir, &base)).await {
        Ok(Ok(files)) => ctx.facts.keep_drafted_files(id, &files),
        Ok(Err(e)) => tracing::warn!("{id}: not keeping the plan's files for the site: {e:#}"),
        Err(e) => tracing::warn!("{id}: not keeping the plan's files for the site: {e}"),
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
        .filter(|t| !is_blank(t))
        .unwrap_or_else(|| format!("Spec: {}", r.title.clone().unwrap_or_else(|| id.to_string())));
    let body = r
        .change_body
        .clone()
        .filter(|b| !is_blank(b))
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
            let why = format!(
                "The spec is committed but couldn't be published: {}",
                reason_or(&format!("{e:#}"), "no reason given")
            );
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
    let revises = revises_of(&draft.other);
    let ids = r.entry_ids(revises.as_deref());
    // A release note is no spec draft: it is reported with no facts and waits for a person.
    let release = draft.other.get("kind").and_then(Value::as_str) == Some("release");
    let (title_of_plan, preview, item_kinds) = plan_files(ctx, draft, dir, r, &ids).await;
    // The plan's files go to the site as a fact while the worktree is still here (`keepDraftFiles` in `drafts.ts`).
    keep_files(ctx, id, dir).await;
    ctx.host.remove(&ctx.project, id);
    let titles = if r.pieces.is_empty() {
        vec![title]
    } else {
        r.pieces.iter().map(|p| p.title.clone()).collect()
    };
    // The item a revision changes may have a build line too, with its new size.
    let revised = revises.as_deref().map(|item| to_revision(r.revision.as_ref(), item));
    let mut shown = ids.clone();
    if let (Some(item), Some(Revised::Keeps { .. })) = (&revises, &revised) {
        shown.push(item.clone());
    }
    let checked = checked_breakdown(r.breakdown.as_ref(), &shown);
    let build_steps = checked.as_ref().map_or(0, |b| b.build.len());
    let breakdown = checked.map(|b| {
        json!({
            "build": b.build.iter().map(|p| json!({"id": p.id, "line": p.line, "size": p.size})).collect::<Vec<_>>(),
            "people": b.people.iter().map(|p| json!({"step": p.step, "who": p.who, "waitOn": p.wait_on})).collect::<Vec<_>>(),
        })
    });
    let facts = (!release).then(|| PlanFacts {
        risk: r.risk.as_deref().is_some_and(|t| !is_blank(t)),
        // The drafting table (drafting-table.md) isn't a thing this agent works out, and the plan's own tier isn't either: the store
        // is told neither and reads a plan with no tier as `tryfirst` for the design review and as waiting when it was an idea's.
        table: false,
        tier: None,
        build: build_steps,
        title: title_of_plan,
        item_kinds,
        preview,
        first: draft.pr_url.is_none(),
    });
    let decided = ctx
        .plan_ready(
            id,
            DraftReport {
                titles: Some(titles),
                summary: Some(r.summary.clone()),
                pr_url: Some(published.url),
                item_ids: (!ids.is_empty()).then_some(ids),
                breakdown,
                plan: facts,
                // Only on a plan that revises an item: what is left of it, or `null` for new work after all.
                revises: revised.as_ref().map(|r| match r {
                    Revised::Keeps { .. } => revises.clone(),
                    Revised::NewWork => None,
                }),
                revision: revised.map(|r| match r {
                    Revised::Keeps { remaining } => Some(Remaining { remaining }),
                    Revised::NewWork => None,
                }),
                ..Default::default()
            },
        )
        .await?;
    // The store's answer is the only word on what happens next: a plan it said nothing about (an older store, or no facts sent)
    // waits for a person. The designs for review are drawn on the idea's own board; a plan that can't draw them still waits for
    // the review, with the reason (design-view/14.3).
    let Some(PlanDecision { approve, needs_design }) = decided else {
        return Ok(());
    };
    if needs_design {
        draw_designs(ctx, id, draft, r).await;
        return Ok(());
    }
    // A plan the store says goes ahead on the yes given when it was asked for: the same Approve a person's press queues, which
    // the land step carries out on its next pass (paused or not) by the project's workflow.
    if approve {
        let Approve { commands, project } = &ctx.approve;
        commands.push_draft(DraftCommand::Approve {
            project: project.clone(),
            draft: id.to_string(),
        });
    }
    Ok(())
}

/// What the store reads of a plan's files (`planFacts.ts`): its title, whether it adds a preview that is safe to draw and the kinds
/// of the items it adds. A worktree that can't be read says no preview and no kinds, never a guess.
async fn plan_files(
    ctx: &Ctx,
    draft: &ClaimedDraft,
    dir: &Path,
    r: &DrafterResult,
    ids: &[String],
) -> (String, bool, Vec<String>) {
    let title = r
        .title
        .clone()
        .or_else(|| draft.titles.first().cloned())
        .unwrap_or_default();
    let (host, project, dir) = (ctx.host.clone(), ctx.project.clone(), dir.to_path_buf());
    let files = match tokio::task::spawn_blocking(move || host.design_files(&project, &dir)).await {
        Ok(Ok(files)) => files,
        _ => return (title, false, Vec::new()),
    };
    let preview = preview_files(&files)
        .iter()
        .any(|f| preview_problem(&f.content).is_none());
    (title, preview, item_kinds(&files, ids).unwrap_or_default())
}

/// Draws the plan's designs and keeps what came of it on the plan: the reason when it couldn't (`designNote`, shown on
/// its card with **Try again**). A report the store doesn't take is logged, and the plan waits for its review all the same.
async fn draw_designs(ctx: &Ctx, id: &str, draft: &ClaimedDraft, r: &DrafterResult) {
    let layouts = |prompt: String| ctx.host.design_layout(&ctx.project, prompt);
    let transport = ctx.host.board_transport();
    let drawing = Drawing {
        client: &ctx.client,
        repo: ctx.repo,
        token: &ctx.token,
        transport: transport.as_ref(),
        layouts: &layouts,
        timing: Default::default(),
        claude_signed_out: ctx.claude_signed_out,
    };
    let title = r
        .title
        .clone()
        .or_else(|| draft.titles.first().cloned())
        .unwrap_or_default();
    let idea = Idea {
        title: &title,
        request: draft.request.as_deref().unwrap_or_default(),
        summary: Some(&r.summary),
        change: None,
    };
    // What the prompt shows beside the designs is kept in the plan's details while this computer still holds the plan.
    let sentences = |prompt: String| ctx.host.design_sentence(&ctx.project, prompt);
    let keeping = Keeping {
        client: &ctx.client,
        repo: ctx.repo,
        token: &ctx.token,
        draft_id: id,
        lease: &ctx.lease,
        sentences: &sentences,
    };
    let (keeping_ref, idea_ref) = (&keeping, &idea);
    let kept = |drawn: Drawn| async move {
        if let Err(e) = keep_review(keeping_ref, idea_ref, &drawn).await {
            ctx.failures.add("draft_details_report", id, &e);
            tracing::warn!("{id}: the designs' review couldn't be kept with the plan's details: {e}");
        }
    };
    if let Err(e) = draw_design_board_with(&drawing, id, &idea, kept).await {
        let note = DraftReport {
            design_note: Some(Some(e.note())),
            ..Default::default()
        };
        if let Err(e) = ctx.report(id, note).await {
            tracing::warn!("{id}: the designs couldn't be drawn, and why didn't reach the store: {e}");
        }
    }
}
