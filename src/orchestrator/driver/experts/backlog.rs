//! Handing an expert session to the backlog (docs/specs/expert-chat.md section 3.8; the TS agent's `endExpert` with
//! `backlog` and `handBack` in `revisions.ts`): the session is parked (its work pushed, its lane free), a plan that
//! revises the item is queued with the conversation in its request, and the session stays as `expert_handed` with the
//! plan's id until it is parked again. When that plan merges the loop gives the lease back, so the store's `revise`
//! finds the item waiting rather than mid-slice, and drops `expert` from it: any computer builds it like any item.
use super::*;
use crate::backlog::facts::QueuedDraft;

/// Parks the session, queues the plan that revises its item and marks the session handed. A plan the store won't
/// queue leaves the session parked, with the reason as the answer.
pub(super) async fn hand_to_backlog(
    cx: &Cx<'_>,
    pool: &mut LanePool,
    item: &str,
    s: &mut Session,
) -> Result<(), String> {
    // Its work pushed first, so the plan can name the branch it builds on.
    park(cx, pool, item, s).await?;
    let request = expert::handoff_request(&s.title, &s.started_by, &s.branch, &s.talk);
    let draft = QueuedDraft {
        request,
        titles: vec![crate::text::truncate(s.title.trim(), 300)],
        revises: Some(item.to_string()),
        requested_by: Some(s.started_by.clone()),
        assignee: Some(s.started_by.clone()),
        ..Default::default()
    };
    let plan = (cx.client.draft_queue(cx.repo, Some(cx.token), &draft).await)
        .map_err(|e| format!("The backlog store wouldn't take the plan: {e}"))?;
    s.detail = Detail::Handed;
    s.plan = Some(plan.clone());
    cx.note(
        item,
        &s.lease,
        &format!("Handed to the backlog: plan {plan} revises it"),
    )
    .await;
    Ok(())
}

impl ProjectLoop {
    /// A plan that revises `item` has merged: when this computer holds it as a handed-off session, gives the lease back
    /// (a history line, then `release`) and forgets the session, before the store's `revise` puts it in the queue. `Ok`
    /// when there was nothing to give back or it is given back; `Err` when the store wouldn't take it yet, so the
    /// revision waits and the next pass tries again.
    pub(in crate::orchestrator::driver) async fn hand_back_expert(
        &mut self,
        repo: u64,
        token: &str,
        item: &str,
        plan: &str,
    ) -> Result<(), String> {
        let Some(s) = self.experts.sessions.get(item).filter(|s| s.detail == Detail::Handed) else {
            return Ok(());
        };
        let (lease, by) = (s.lease.clone(), s.started_by.clone());
        let note = format!("Handed to the backlog by {by}");
        let gave = async {
            self.client.report(repo, Some(token), item, &lease, &note).await?;
            self.client.release(repo, Some(token), item, &lease).await
        };
        match gave.await {
            // The lease is gone already (it lapsed, or another computer's): the item is the store's to revise.
            Err(e) if e.code() != Some("stale_lease") => return Err(format!("couldn't give {item} back: {e}")),
            _ => {}
        }
        tracing::info!("{}: plan {plan} approved: expert {item} goes on as any item", self.id);
        self.experts.sessions.remove(item);
        if let Some(queue) = &self.experts.queue {
            queue.clear(item).await;
        }
        self.publish_experts();
        Ok(())
    }
}
