//! What a person's press on a plan's design review does (TS `drafts.ts` `skipDesign`, `drawing`, `drawDesign`): the
//! functions the `draft_skip_design` and `board_design_write` command arms call. Each takes the draft from the store
//! (the same `draft_take` a press on **Approve** makes), checks it is waiting for approval and for its design review,
//! reports what changed with `draft_report` and lets go of it again.
//!
//! The review's state is `needsDesign` and `designNote` on the store's draft: set by the report that makes the plan
//! ready, cleared by **Skip the design** or **Approve design**, read back here from the draft the store hands over.
use serde_json::Value;

use super::draw::{Drawing, Layouts, draw_design_board_with};
use super::error::DesignError;
use super::keep::{Keeping, keep_review};
use super::needs_design::{design_note, design_pending};
use super::session::Idea;
use crate::backlog::ClaimNextAgent;
use crate::backlog::drafts::{ClaimedDraft, DraftReport, TakenDraft};

/// What the arms hand over: how to reach the store and the board, and who this computer is to the store.
pub struct Acting<'a> {
    pub drawing: Drawing<'a>,
    /// The session that writes a sentence again when the first failed its check.
    pub sentences: Layouts<'a>,
    pub agent: &'a ClaimNextAgent,
}

fn waiting_for_approval(t: &TakenDraft) -> bool {
    let text = |k: &str| t.draft.other.get(k).and_then(Value::as_str);
    text("status") == Some("needs_person") && text("detail") == Some("awaiting_approval")
}

fn idea_of(d: &ClaimedDraft) -> (String, String, Option<String>) {
    let title = d
        .titles
        .first()
        .cloned()
        .or_else(|| d.other.get("title").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    let summary = d.other.get("summary").and_then(Value::as_str).map(str::to_string);
    (title, d.request.clone().unwrap_or_default(), summary)
}

impl Acting<'_> {
    /// Takes the draft for a press, or says why not.
    async fn take(&self, id: &str) -> Result<TakenDraft, DesignError> {
        let a = &self.drawing;
        let taken = a.client.draft_take(a.repo, Some(a.token), id, self.agent).await?;
        if waiting_for_approval(&taken) {
            return Ok(taken);
        }
        self.release(id, &taken.token).await;
        Err(DesignError::conflict(format!("Draft {id} isn't waiting for approval.")))
    }

    async fn release(&self, id: &str, lease: &str) {
        let a = &self.drawing;
        if let Err(e) = a.client.draft_release(a.repo, Some(a.token), id, lease).await {
            tracing::warn!("couldn't let go of draft {id}: {e}");
        }
    }

    async fn report(&self, id: &str, lease: &str, report: DraftReport) -> Result<(), DesignError> {
        let a = &self.drawing;
        a.client
            .draft_report(a.repo, Some(a.token), id, lease, &report)
            .await
            .map_err(Into::into)
    }
}

/// **Skip the design** (`draft_skip_design`, design-view.md section 11): a plan waiting for a design review loses its
/// flag and note and goes on to **Approve plan**. It is the store's own op, `draft_skip_design { id }`, which needs no
/// lease: whoever holds the plan, or no one, it is cleared there. A plan with no review to skip is left as it is, and a
/// finished one is refused by the store. The TS agent also adds a "Design skipped" line to the plan's timeline; the
/// store's draft has no such line to write to here.
pub async fn skip_design(a: &Acting<'_>, id: &str) -> Result<(), DesignError> {
    let d = &a.drawing;
    d.client
        .draft_skip_design(d.repo, Some(d.token), id)
        .await
        .map_err(Into::into)
}

/// `board_design_write` (design-view/14.3): draws, or redraws with the person's `note` (**Change it…**), the designs of a
/// plan that waits for its design review, and keeps what came of it on the plan: the reason when it couldn't, cleared
/// once a drawing lands. Fails with the reason.
pub async fn draw_design(a: &Acting<'_>, id: &str, note: Option<&str>) -> Result<(), DesignError> {
    let taken = a.take(id).await?;
    if !design_pending(&taken.draft.other) {
        a.release(id, &taken.token).await;
        return Err(DesignError::conflict("This plan has no design to review."));
    }
    let (title, request, summary) = idea_of(&taken.draft);
    let idea = Idea {
        title: &title,
        request: &request,
        summary: summary.as_deref(),
        change: note,
    };
    let d = &a.drawing;
    let keeping = Keeping {
        client: d.client,
        repo: d.repo,
        token: d.token,
        draft_id: id,
        lease: &taken.token,
        sentences: a.sentences,
        kit: d.kit,
    };
    // The review goes into the plan's details while this computer holds it, before it is let go.
    let (keeping_ref, idea_ref) = (&keeping, &idea);
    let kept = |drawn| async move {
        if let Err(e) = keep_review(keeping_ref, idea_ref, &drawn).await {
            tracing::warn!("{id}: the designs' review couldn't be kept with the plan's details: {e}");
        }
    };
    let drawn = draw_design_board_with(d, id, &idea, kept).await;
    let kept = match &drawn {
        Ok(()) if design_note(&taken.draft.other).is_some() => Some(None),
        Ok(()) => None,
        Err(e) => Some(Some(e.note())),
    };
    if let Some(design_note) = kept {
        let report = DraftReport {
            design_note: Some(design_note),
            ..Default::default()
        };
        if let Err(e) = a.report(id, &taken.token, report).await {
            tracing::warn!("couldn't keep the design note on draft {id}: {}", e.message);
        }
    }
    a.release(id, &taken.token).await;
    drawn
}
