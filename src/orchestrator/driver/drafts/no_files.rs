//! A plan that changed no file (TS `noFiles` in `agent/src/drafts.ts`; a board plan that only links to the specs failed with "No
//! commits between main and ...", 2026-10-25). It has nothing to open a pull request for, so its items go straight to the
//! backlog as a merged plan's do: listed and marked merged here (`plan_items.rs`, which sends them to the store), and the draft
//! finishes `merged` with its title, summary, entries and breakdown, no files and no pull request. No plan facts go with it, so
//! the store has nothing to approve; it never reaches `awaiting_approval`. A plan that changed no file and planned no item
//! has nothing to approve either, and fails.
use std::path::Path;

use serde_json::{Value, json};

use super::{BacklogError, ClaimedDraft, Ctx, DraftReport, DrafterResult};
use crate::drafts::items::{checked_items, with_milestones};
use crate::drafts::model::checked_breakdown;
use crate::drafts::revision::{Revised, revises_of, to_revision};

/// The reason a plan that changed no file and planned no item fails with.
pub(super) const NOTHING_TO_APPROVE: &str =
    "The drafter changed no file and planned no item, so there is nothing to approve.";

/// Whether the worktree changed no file against the base. A worktree that can't be read says it changed some (TS `[{}]`).
pub(super) async fn changed_nothing(ctx: &Ctx, dir: &Path) -> bool {
    let (host, project, dir) = (ctx.host.clone(), ctx.project.clone(), dir.to_path_buf());
    tokio::task::spawn_blocking(move || host.changed_nothing(&project, &dir))
        .await
        .is_ok_and(|r| r.unwrap_or(false))
}

/// The failure of a plan that changed no file and planned no item.
pub(super) async fn nothing_to_approve(ctx: &Ctx, id: &str) -> Result<(), BacklogError> {
    let report = DraftReport {
        summary: Some(NOTHING_TO_APPROVE.to_string()),
        ..Default::default()
    };
    ctx.failed(id, report).await
}

/// A plan with items that changed no file: its items straight to the backlog. `None` when it changed some file or its items
/// check to none (then it is published as usual); otherwise the draft's end, as the store took it.
pub(super) async fn straight_to_backlog(
    ctx: &Ctx,
    id: &str,
    draft: &ClaimedDraft,
    dir: &Path,
    r: &DrafterResult,
) -> Option<Result<(), BacklogError>> {
    if r.items.is_empty() || !changed_nothing(ctx, dir).await {
        return None;
    }
    let revises = revises_of(&draft.other);
    let checked = checked_items(&r.items, revises.as_deref(), false);
    let dir_owned = dir.to_path_buf();
    let items = tokio::task::spawn_blocking(move || with_milestones(checked, &dir_owned))
        .await
        .unwrap_or_default();
    if items.is_empty() {
        return None;
    }
    let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
    let mut shown = ids.clone();
    if let Some(item) = &revises
        && let Revised::Keeps { .. } = to_revision(r.revision.as_ref(), item)
    {
        shown.push(item.clone());
    }
    let breakdown = checked_breakdown(r.breakdown.as_ref(), &shown).map(|b| {
        json!({
            "build": b.build.iter().map(|p| json!({"id": p.id, "line": p.line, "size": p.size})).collect::<Vec<_>>(),
            "people": b.people.iter().map(|p| json!({"step": p.step, "who": p.who, "waitOn": p.wait_on})).collect::<Vec<_>>(),
        })
    });
    ctx.host.remove(&ctx.project, id);
    // Listed and merged before the report, so a crash right after it still sends them.
    let asker = draft.other.get("requestedBy").and_then(Value::as_str);
    super::super::plan_items::keep(&ctx.items, id, items, asker);
    super::super::plan_items::merged(&ctx.items, id, asker);
    let title = r.title.clone().or_else(|| draft.titles.first().cloned());
    Some(
        ctx.report_final(
            id,
            DraftReport {
                status: Some("merged".into()),
                detail: Some("merged".into()),
                titles: title.map(|t| vec![t]),
                summary: Some(r.summary.clone()),
                item_ids: Some(ids),
                breakdown,
                ..Default::default()
            },
        )
        .await,
    )
}
