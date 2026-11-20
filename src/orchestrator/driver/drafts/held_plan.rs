//! The plan as the store holds it after the report that makes it ready, for an Approve that goes ahead on the lease held now.
use serde_json::{Value, json};

use super::{ClaimedDraft, Ctx, TakenDraft};

/// The plan as the store holds it after the report that was just sent, with the lease: what `draft_take` would have handed
/// `land` (its status, PR, branch, items, requester, the revision's remaining and no design review).
pub(super) fn held_plan(
    ctx: &Ctx,
    draft: &ClaimedDraft,
    pr_url: &str,
    titles: &[String],
    ids: &[String],
    remaining: Option<String>,
) -> TakenDraft {
    let mut draft = draft.clone();
    draft.pr_url = Some(pr_url.to_string());
    draft.titles = titles.to_vec();
    let mut set = |k: &str, v: Value| draft.other.insert(k.into(), v);
    set("status", json!("needs_person"));
    set("detail", json!("awaiting_approval"));
    set("needsDesign", json!(false));
    if !ids.is_empty() {
        set("itemIds", json!(ids));
    }
    if let Some(remaining) = remaining {
        set("revision", json!({ "remaining": remaining }));
    }
    TakenDraft {
        draft,
        token: ctx.lease.clone(),
    }
}
