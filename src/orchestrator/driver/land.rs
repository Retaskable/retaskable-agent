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
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::task::JoinHandle;

use super::draft_owed::{Kept, Sender};
use super::drafts::{DraftHost, DraftJob, claim_agent, rebase_request};
use super::start_gate::StartGate;
use super::{ProjectLoop, Shared};
use crate::backlog::Client;
use crate::backlog::drafts::{DraftReport, TakenDraft};
use crate::backlog::revise::Owed;
use crate::config::ProjectConfig;
use crate::design_board::{NEEDS_DESIGN_FIRST, design_pending};
use crate::drafts::DrafterStatus;
use crate::drafts::land::{PrState, deletes_head, pr_number, workflow};
use crate::drafts::revision::revises_of;
use crate::engines::{EngineId, Role};
use crate::view::{DraftCommand, Reply};

/// The wait between the tries at the merge once the branch is rebased. How many tries, and how many times a conflicted draft
/// goes back to its drafter to rebase, are the store's (`draftMergeTries`, `draftHandBacks`).
pub(super) const MERGE_WAIT: Duration = Duration::from_secs(2);

/// How often the plans waiting for approval have their pull requests looked at: one `gh` call each, so not every pass
/// (the TS agent's `LANDED_EVERY_MS`).
pub(super) const LANDED_EVERY: Duration = Duration::from_secs(10 * 60);

/// What a merged draft adds to the next `upsert_from_specs`.
pub(super) struct Landed {
    item_ids: Vec<String>,
    asked_by: Option<String>,
    draft_id: String,
}

/// The presses being carried out, and what merged drafts have yet to tell the store.
#[derive(Default)]
pub(super) struct Landing {
    pub(super) tasks: Vec<JoinHandle<Option<Landed>>>,
    approved: Vec<String>,
    asked_by: BTreeMap<String, String>,
    /// Merged plans' revisions the store hasn't taken yet, by plan id (kept in the state file, `revise.rs`).
    pub(super) revisions: BTreeMap<String, Owed>,
    /// When the plans waiting for approval were last checked against the code host, and how often they are.
    pub(super) looked: Option<Instant>,
    pub(super) look_every: Duration,
}

impl Landing {
    pub(super) fn with_revisions(revisions: BTreeMap<String, Owed>) -> Self {
        Self {
            revisions,
            look_every: LANDED_EVERY,
            ..Default::default()
        }
    }

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
    failures: crate::backlog::failures::Failures,
    /// Final reports the store hasn't taken.
    kept: Kept,
    /// The items each plan adds, listed until the store holds them (`plan_items.rs`).
    items: super::plan_items::Kept,
    project: ProjectConfig,
    host: Arc<dyn DraftHost>,
    wait: Duration,
    /// The engine to start on when it isn't the drafter's chosen one.
    engine: Option<EngineId>,
    sessions: crate::orchestrator::live::sessions::Sessions,
    /// A `merge_requested` row waiting on how a Merge into main ended.
    reply: Option<Reply>,
}

impl Ctx {
    /// Tells the row waiting on this press how it ended, if one is.
    fn answer(&self, answer: Result<(), String>) {
        if let Some(reply) = &self.reply {
            reply.send(answer);
        }
    }

    async fn report(&self, id: &str, report: DraftReport) {
        let sent = (self.client).draft_report(self.repo, Some(&self.token), id, &self.lease, &report);
        if let Err(e) = sent.await {
            tracing::warn!("a draft's report didn't reach the store: {e}");
            self.failures.add("draft_report", id, &e);
        }
    }

    /// A draft's last word, `merged` or `discarded`: the change is already made, so the store must hear of it (the
    /// owner's outage rule, 2026-11-16: "it should be able to finish what it's doing but it can't pick up the next
    /// thing till the store is back."). Kept in the project's folder and sent again every [`Self::wait`] while the
    /// store can't be reached or answers 5xx, also after a restart, for up to a day; a refusal that names a code (the
    /// draft moved on: `stale_lease`) drops it (`draft_owed.rs`). An improvement over the TS agent (`draftMove.ts`),
    /// which sends once and ignores a failure.
    async fn report_final(&self, id: &str, report: DraftReport, revision: Option<Owed>) {
        let sender = Sender {
            client: &self.client,
            repo: self.repo,
            token: &self.token,
            failures: &self.failures,
            kept: &self.kept,
            wait: self.wait,
        };
        let _ = sender.send(id, &self.lease, &report, revision).await;
    }

    /// Lets go of a draft taken for a press that can't be carried out.
    async fn release(&self, id: &str) {
        if let Err(e) = (self.client)
            .draft_release(self.repo, Some(&self.token), id, &self.lease)
            .await
        {
            tracing::warn!("couldn't let go of draft {id}: {e}");
            self.failures.add("draft_release", id, &e);
        }
    }

    /// Back to waiting for approval, the reason on its timeline.
    async fn back(&self, id: &str, why: &str) {
        self.noted(id, format!("Couldn't merge: {why}")).await;
    }

    /// Back to waiting for approval with `note` on its timeline.
    async fn noted(&self, id: &str, note: String) {
        let report = DraftReport {
            status: Some("needs_person".into()),
            detail: Some("awaiting_approval".into()),
            summary: Some(note),
            ..Default::default()
        };
        self.report(id, report).await;
    }

    /// `gh pr merge`, up to `tries` times; `squash` forces `--squash`, the settings' method flag otherwise.
    async fn merge(&self, pr: u64, delete_branch: bool, squash: bool, tries: u32) -> Result<(), String> {
        let mut last = String::new();
        for i in 0..tries {
            if i > 0 {
                tokio::time::sleep(self.wait).await;
            }
            let (host, project) = (self.host.clone(), self.project.clone());
            let done = tokio::task::spawn_blocking(move || host.merge(&project, pr, delete_branch, squash)).await;
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
            engine: self.engine,
            sessions: self.sessions.clone(),
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

/// What the row of a Merge into main is refused with when the draft can't be merged by a person (`mergeOwn`).
fn not_waiting(id: &str) -> String {
    format!("Draft {id} isn't waiting for you to merge it.")
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
    ctx.items.remove(&id);
    ctx.report_final(&id, report, None).await;
    None
}

/// One press, from taking the draft to reporting it. `Some` when the pull request merged.
async fn land(ctx: Ctx, id: String, merge_press: bool, taken: TakenDraft) -> Option<Landed> {
    let wf = workflow(&ctx.project);
    let draft = &taken.draft;
    // The tries at the merge and the hand-backs are the store's numbers; until it has sent them nothing is pressed.
    let Some(tuning) = ctx.project.tuning.store else {
        tracing::info!("dropped a press on {id}: the store's numbers aren't known yet");
        ctx.release(&id).await;
        ctx.answer(Err(
            "The store's numbers aren't known yet; try again in a minute.".into()
        ));
        return None;
    };
    let pr = draft.pr_url.as_deref().and_then(pr_number);
    let waiting = matches!(
        (text(&taken, "status"), text(&taken, "detail")),
        (Some("needs_person"), Some("awaiting_approval" | "awaiting_merge"))
    );
    let (Some(pr), true) = (pr, waiting) else {
        tracing::info!("dropped a press on {id}: it isn't waiting for approval here");
        ctx.release(&id).await;
        ctx.answer(Err(not_waiting(&id)));
        return None;
    };
    // A plan that waits for its design review is not approved: review the designs, or skip the design first
    // (`approveDraft`). The store's copy says so; one that can't say is no reason to hold a plan.
    if !merge_press && design_pending(&draft.other) {
        tracing::info!("dropped an Approve on {id}: it needs a design review first");
        ctx.release(&id).await;
        ctx.answer(Err(NEEDS_DESIGN_FIRST.to_string()));
        return None;
    }
    let own_branch = draft.branch.as_deref().is_some_and(deletes_head);
    let delete = wf.delete_branch && own_branch;
    if merge_press {
        let Some(into) = &wf.person_merges_into else {
            tracing::info!("dropped a Merge into main on {id}: this project's pattern doesn't have it");
            ctx.release(&id).await;
            ctx.answer(Err(not_waiting(&id)));
            return None;
        };
        // A person's merge is always squashed, and deletes the draft's own branch whatever the settings say (`forPerson`).
        if let Err(why) = ctx.merge(pr, own_branch, true, 1).await {
            let why = crate::team_branch::gh_words(&why);
            ctx.noted(&id, format!("Couldn't merge into {into}: {why}")).await;
            ctx.answer(Err(format!("Couldn't merge #{pr}: {why}")));
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
    } else if let Err(first) = ctx.merge(pr, delete, false, 1).await {
        // Behind the base, perhaps: rebase it when that's clean, and merge once GitHub sees the push.
        let base = &ctx.project.base_branch;
        let Some(branch) = draft.branch.as_deref() else {
            ctx.back(&id, &first).await;
            return None;
        };
        let mut hand_backs = 0;
        let why = loop {
            match ctx.rebase(&id, branch).await {
                Ok(true) => break ctx.merge(pr, delete, false, tuning.draft_merge_tries).await.err(),
                Ok(false) if hand_backs < tuning.draft_hand_backs => {
                    hand_backs += 1;
                    let reason = format!("it conflicts with {base}");
                    if let Err(e) = ctx.hand_back(&taken, branch, &reason).await {
                        break Some(format!("{reason}, and the drafter couldn't rebase it: {e}"));
                    }
                }
                Ok(false) => {
                    break Some(format!(
                        "it conflicts with {base}, even after going back to the drafter {} times. Send it back with notes.",
                        tuning.draft_hand_backs
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
    let landed = merged(&ctx, id, &taken).await;
    ctx.answer(Ok(()));
    Some(landed)
}

/// A draft whose pull request is merged, by this agent or by a person on the code host: the worktree goes, the remote
/// is fetched, the store is told it's `merged`, and what the plan adds to the backlog is handed back (its items, and
/// the revision of the item it revises).
async fn merged(ctx: &Ctx, id: String, taken: &TakenDraft) -> Landed {
    let draft = &taken.draft;
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
    // A plan that revises an item gives it back to the queue with what is left (`storeRevision`): the store's draft
    // says which item and, once the plan was written, what remains.
    let revision = revises_of(&draft.other).zip(
        (draft.other.get("revision").and_then(|r| r.get("remaining")))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|r| !r.is_empty()),
    );
    let revision = revision.map(|(item, remaining)| Owed {
        item,
        remaining: remaining.to_string(),
        title: draft.titles.first().cloned().unwrap_or_default(),
        by: text(taken, "requestedBy").map(String::from),
        since: crate::owed_file::now_secs(),
    });
    // The plan's own items are owed to the store from here on, listed before the report is sent (`plan_items.rs`).
    super::plan_items::merged(&ctx.items, &id, text(taken, "requestedBy"));
    // Kept with the report, so the item is revised after a restart too (`draft_owed.rs`, `revise.rs`).
    ctx.report_final(&id, report, revision).await;
    let item_ids: Vec<String> = (draft.other.get("itemIds").and_then(Value::as_array))
        .map(|l| l.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Landed {
        item_ids,
        asked_by: text(taken, "requestedBy").map(String::from),
        draft_id: id,
    }
}

/// A plan a person merged or closed on the code host while it waited for approval (the TS agent's `draftsLanded`):
/// merged, it ends as one this agent merged does (`merged`), and closed, as a Discard does (`discarded`, the pull request
/// already being closed). Taking the draft is the lock: it was found held by no computer, and the store hands it to the
/// first to take it.
async fn landed(ctx: Ctx, id: String, state: PrState, taken: TakenDraft) -> Option<Landed> {
    if state == PrState::Merged {
        return Some(merged(&ctx, id, &taken).await);
    }
    let (host, project, draft_id) = (ctx.host.clone(), ctx.project.clone(), id.clone());
    let _ = tokio::task::spawn_blocking(move || host.remove(&project, &draft_id)).await;
    let report = DraftReport {
        status: Some("discarded".into()),
        detail: Some("discarded".into()),
        ..Default::default()
    };
    ctx.items.remove(&id);
    ctx.report_final(&id, report, None).await;
    None
}

/// Spawns a landing task that raises the project loop's wake when it ends, so the loop reads what it did (the merge's
/// headers, the next piece) now and not at the next beat.
fn kicking<F>(shared: &Shared<'_>, work: F) -> JoinHandle<Option<Landed>>
where
    F: Future<Output = Option<Landed>> + Send + 'static,
{
    let kick = shared.links.work.kick_handle();
    tokio::spawn(async move {
        let landed = work.await;
        kick.notify_one();
        landed
    })
}

impl ProjectLoop {
    fn land_ctx(
        &self,
        repo: u64,
        token: &str,
        lease: &str,
        host: &Arc<dyn DraftHost>,
        wait: Duration,
        reply: Option<Reply>,
    ) -> Ctx {
        Ctx {
            client: self.client.clone(),
            repo,
            token: token.to_string(),
            lease: lease.to_string(),
            failures: self.failures.clone(),
            kept: self.drafts_owed.clone(),
            items: self.plan_items.clone(),
            project: self.project.clone(),
            host: host.clone(),
            wait,
            engine: None,
            sessions: self.live.sessions().clone(),
            reply,
        }
    }

    /// Every [`Landing::look_every`], looks at the pull request of each plan waiting for approval that no computer holds:
    /// one a person merged on the code host is taken (`draft_take`, the lock) and ends as a merged one does, so the
    /// item it revises goes back to the queue; one closed there is `discarded`. A store that can't be asked leaves it for
    /// the next pass (nothing is taken meanwhile, the outage rule), and a pull request that can't be read is looked at
    /// again later. Paused or not, in both modes.
    async fn settle_landed(&mut self, host: &Arc<dyn DraftHost>, shared: &Shared<'_>, wait: Duration) {
        let due = self
            .landing
            .looked
            .is_none_or(|at| at.elapsed() >= self.landing.look_every);
        if !due || shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        let rows = match self.client.drafts(repo, Some(&token)).await {
            Ok(rows) => rows,
            // Not looked at: the next pass asks again.
            Err(e) => return self.store_error("drafts", &e),
        };
        self.landing.looked = Some(Instant::now());
        for row in rows {
            let waiting = row.status.as_deref() == Some("needs_person")
                && row.detail.as_deref() == Some("awaiting_approval")
                && row.held_by.is_none();
            let Some(pr) = row.pr_url.as_deref().and_then(pr_number).filter(|_| waiting) else {
                continue;
            };
            let (h, project) = (host.clone(), self.project.clone());
            let state = match tokio::task::spawn_blocking(move || h.pr_state(&project, pr)).await {
                Ok(Ok(PrState::Open)) => continue,
                Ok(Ok(state)) => state,
                Ok(Err(e)) => {
                    tracing::warn!(
                        "{}: checking {}'s pull request failed, trying again later: {e:#}",
                        self.id,
                        row.id
                    );
                    continue;
                }
                Err(_) => continue,
            };
            let agent = claim_agent(shared.links, &self.project);
            let taken = match self.client.draft_take(repo, Some(&token), &row.id, &agent).await {
                Ok(taken) => taken,
                // Someone else took it (or it moved on): they end it.
                Err(e) => {
                    tracing::warn!(
                        "{}: couldn't take draft {} whose pull request landed: {e}",
                        self.id,
                        row.id
                    );
                    self.store_failure("draft_take", &row.id, &e).await;
                    continue;
                }
            };
            let word = if state == PrState::Merged { "merged" } else { "closed" };
            tracing::info!(
                "{}: draft {}'s pull request was {word} while it waited for approval",
                self.id,
                row.id
            );
            let ctx = self.land_ctx(repo, &token, &taken.token, host, wait, None);
            self.landing
                .tasks
                .push(kicking(shared, landed(ctx, row.id, state, taken)));
        }
    }

    /// Takes in this project's queued presses and starts each; tells the store the headers of the drafts that merged
    /// since the last pass, paused or not.
    pub(super) async fn land_drafts(
        &mut self,
        host: &Arc<dyn DraftHost>,
        shared: &Shared<'_>,
        wait: Duration,
        lanes: u32,
    ) {
        let mut merged = false;
        for task in std::mem::take(&mut self.landing.tasks) {
            if !task.is_finished() {
                self.landing.tasks.push(task);
            } else if let Ok(Some(landed)) = task.await {
                tracing::info!("{}: merged draft {}", self.id, landed.draft_id);
                self.held.ended(&landed.draft_id);
                for id in &landed.item_ids {
                    if let Some(by) = &landed.asked_by {
                        self.landing.asked_by.insert(id.clone(), by.clone());
                    }
                }
                self.landing.approved.extend(landed.item_ids);
                merged = true;
            }
        }
        self.list_kept_revisions();
        self.send_revisions(shared).await;
        if merged {
            // The merge is fetched already; the headers go now, not on the minute's fetch.
            self.report(shared.links).await;
        }
        // The plans' own items, after the headers: the store adds the ones it lacks (`plan_items.rs`).
        self.send_plan_items(shared).await;
        self.free_held(shared).await;
        self.free_own_held(shared).await;
        self.settle_landed(host, shared, wait).await;
        let presses = shared.links.item_commands.take_drafts(&self.id);
        if presses.is_empty() || shared.stopping {
            return;
        }
        // A press on a plan's design review needs no engine slot (a skip is the store's own op) and starts its own task.
        let (designs, presses): (Vec<_>, Vec<_>) = presses.into_iter().partition(DraftCommand::is_design);
        self.design_presses(designs, host, shared).await;
        if presses.is_empty() {
            return;
        }
        let gate = StartGate::for_lanes(self, lanes, shared);
        // A rebase resumes the drafter's own session: it waits while that engine is out and never moves.
        if gate.role_start(self, shared, Role::Drafter, false).is_none() {
            return;
        }
        let Some(repo) = self.repo else { return };
        let token = match shared.links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        for press in presses {
            let (DraftCommand::Approve { draft, .. }
            | DraftCommand::Merge { draft, .. }
            | DraftCommand::MergeFor { draft, .. }
            | DraftCommand::Discard { draft, .. }) = &press
            else {
                continue;
            };
            let id = draft.clone();
            let merge_press = matches!(press, DraftCommand::Merge { .. } | DraftCommand::MergeFor { .. });
            let reply = match &press {
                DraftCommand::MergeFor { reply, .. } => Some(reply.clone()),
                _ => None,
            };
            let agent = claim_agent(shared.links, &self.project);
            // A plan the drafter just wrote and the store approved is still held: it goes ahead on that lease, the store
            // refuses `draft_take` of a held draft.
            let taken = match self.held.take_approved(&id) {
                Some(held) => held,
                None => match self.client.draft_take(repo, Some(&token), &id, &agent).await {
                    Ok(taken) => taken,
                    Err(e) => {
                        tracing::warn!("{}: couldn't take draft {id}: {e}", self.id);
                        if let Some(reply) = &reply {
                            reply.send(Err(format!("Couldn't take draft {id}: {e}")));
                        }
                        self.store_failure("draft_take", &id, &e).await;
                        continue;
                    }
                },
            };
            let ctx = self.land_ctx(repo, &token, &taken.token, host, wait, reply);
            let task = if matches!(press, DraftCommand::Discard { .. }) {
                kicking(shared, discard(ctx, id, taken))
            } else {
                kicking(shared, land(ctx, id, merge_press, taken))
            };
            self.landing.tasks.push(task);
        }
    }
}
