//! A plan's designs (docs/specs/project-kits.md): the drafter's own `design`, kept as the plan's review from the project's kit
//! (`plan_design`), and the drawing on the idea's board when the store says the plan waits for its review (`draw_designs`).
use serde_json::Value;

use super::{ClaimedDraft, Ctx, DrafterResult};
use crate::backlog::drafts::DraftReport;
use crate::design_board::needs_design::preview_files;
use crate::design_board::session::Idea;
use crate::design_board::{Drawing, Drawn, Keeping, PlanDesign, draw_design_board_with, keep_plan_design, keep_review};
use crate::drafts::files::DraftFile;
use crate::kit::Kit;

impl Ctx {
    /// The project's kit (`kit::store::kit_for`): its own pieces, the plain wireframe one, or Retaskable's library.
    pub(super) async fn kit(&self) -> Kit {
        self.host
            .project_kit(
                &self.project,
                self.client.clone(),
                self.repo,
                self.token.clone(),
                self.machine.clone(),
            )
            .await
    }
}

/// The plan's files without a new drawing the drafter wrote (a drawing is not a design here), and whether its `design` was kept:
/// the `preview` fact the store decides a table and a design review from. A plan sent back with no new design keeps the one it has.
pub(super) async fn plan_design(
    ctx: &Ctx,
    id: &str,
    r: &DrafterResult,
    files: Vec<DraftFile>,
    kit: &Kit,
) -> (Vec<DraftFile>, bool) {
    let drawn: Vec<String> = preview_files(&files)
        .iter()
        .filter(|f| f.is_new)
        .map(|f| f.path.clone())
        .collect();
    if !drawn.is_empty() {
        tracing::info!("{id}: a drawing is not a design here: {} left out", drawn.join(", "));
    }
    let files: Vec<DraftFile> = files.into_iter().filter(|f| !drawn.contains(&f.path)).collect();
    let sentences = |prompt: String| ctx.host.design_sentence(&ctx.project, prompt);
    let keeping = Keeping {
        client: &ctx.client,
        repo: ctx.repo,
        token: &ctx.token,
        draft_id: id,
        lease: &ctx.lease,
        sentences: &sentences,
        kit,
    };
    if let Some(design) = r.design.as_ref().filter(|d| !d.is_null()) {
        match keep_plan_design(&keeping, design).await {
            Ok(PlanDesign::Kept(_)) => return (files, true),
            Ok(PlanDesign::Dropped(why)) => tracing::info!("{id}: {why}"),
            Err(e) => {
                ctx.failures.add("draft_details_report", id, &e);
                tracing::warn!("{id}: the drafter's design couldn't be kept: {e}");
            }
        }
    }
    let held = match ctx.client.draft_details_get(ctx.repo, Some(&ctx.token), id).await {
        Ok(record) => record.details.get("designReview").is_some_and(Value::is_string),
        Err(_) => false,
    };
    (files, held)
}

/// Draws the plan's designs and keeps what came of it on the plan: the reason when it couldn't (`designNote`, shown on
/// its card with **Try again**). A report the store doesn't take is logged, and the plan waits for its review all the same.
pub(super) async fn draw_designs(ctx: &Ctx, id: &str, draft: &ClaimedDraft, r: &DrafterResult) {
    let kit = ctx.kit().await;
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
        kit: &kit,
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
        kit: &kit,
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
