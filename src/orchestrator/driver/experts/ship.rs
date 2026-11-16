//! Shipping an expert session (docs/specs/expert-chat.md section 3.8; the TS agent's `shipExpert` in
//! `expert/commands.ts`, through `changeOpened`): what is uncommitted is committed, the work pushed as the item's own
//! branch and its change opened, as an implementer's is. The session then ends and the item goes on as any item does:
//! this computer keeps its lease and its lane, and the project's item loop takes it over in `checking`, so checks,
//! review and merge are the loop's own (`ProjectLoop::adopt_shipped`).
//!
//! The store hears `report { status: "in_review", detail: "expert_shipped", branch, prUrl }` before anything is pushed
//! (so a restart always finds the item again: `read.rs` lets an item with that detail through), and again with the
//! change's address once it is open. A change that can't be opened rolls the marker back and leaves the session as it
//! was. This is `claim` mode's ship; in `runs` mode no loop holds an item, and `ship_runs.rs` hands the change to the
//! store as a piece.
use super::*;
use crate::backlog::expert::Shipped as Marker;
use crate::item_lifecycle::ItemState;

/// A session whose change is open: what the item loop takes over.
pub(super) struct Shipped {
    item: crate::backlog::Item,
    lease: String,
    lane: u32,
}

/// The change's description (`shipExpert`): the session's request, then who drove it.
pub(super) fn body(request: &str, started_by: &str) -> String {
    format!(
        "{}\n\nDriven by {started_by} in a Retaskable expert session.",
        crate::text::truncate(request.trim(), 4000)
    )
}

pub(super) async fn ship(cx: &Cx<'_>, pool: &mut LanePool, item: &str, s: &mut Session) -> Result<Shipped, String> {
    // A parked session's work is pushed, but its change opens from a lane.
    let (lane, took) = match s.lane {
        Some(lane) => (lane, false),
        None => match pool.claim_added(cx.holder(item)) {
            Some(lane) => (lane, true),
            None => return Err(expert::ADDED_LANE_TAKEN.into()),
        },
    };
    let give_lane = |pool: &mut LanePool| {
        if took {
            pool.release(lane, &cx.holder(item));
        }
    };
    let target = crate::orchestrator::session::branch_name(&s.item, 0);
    let marker = |status, detail, pr_url| Marker {
        status,
        detail,
        branch: &target,
        pr_url,
    };
    let told = marker(expert::SHIPPED_STATUS, expert::SHIPPED, None);
    if let Err(e) = (cx.client.report_shipped(cx.repo, Some(cx.token), item, &s.lease, &told)).await {
        give_lane(pool);
        return Err(format!("The backlog store wouldn't take the change: {e}"));
    }
    let job = ShipJob {
        project: cx.project.clone(),
        lane,
        branch: s.branch.clone(),
        target: target.clone(),
        prepare: took || !s.prepared,
        title: format!("{item}: {}", s.title),
        body: body(&s.request, &s.started_by),
    };
    let published = match cx.host.ship(job).await {
        Ok(opened) => opened.change,
        Err(why) => {
            // The change isn't open: the session is as it was, and the store is told so.
            let undone = marker("claimed", "", None);
            if let Err(e) = (cx
                .client
                .report_shipped(cx.repo, Some(cx.token), item, &s.lease, &undone))
            .await
            {
                tracing::warn!("{}: expert {item}: the store still says it was shipped: {e}", cx.id);
            }
            give_lane(pool);
            return Err(format!("Couldn't push {} or open its pull request: {why}", s.branch));
        }
    };
    let open = marker(expert::SHIPPED_STATUS, expert::SHIPPED, Some(&published.url));
    if let Err(e) = (cx.client.report_shipped(cx.repo, Some(cx.token), item, &s.lease, &open)).await {
        tracing::warn!(
            "{}: expert {item}: the change's address didn't reach the store: {e}",
            cx.id
        );
    }
    cx.note(item, &s.lease, "Shipped by the person").await;
    // Its queued messages and its continuation note go with the session.
    cx.queue.clear(item).await;
    Ok(Shipped {
        item: s.item.clone(),
        lease: s.lease.clone(),
        lane,
    })
}

impl ProjectLoop {
    /// Takes a shipped session's item over as an item of this project's own loop, in `checking` on the lane the
    /// session held, under the lease it held: from here its checks, review and merge are the loop's (as after an
    /// implementer's `changeOpened`), and a restart takes it back from the saved record as any item's.
    pub(super) fn adopt_shipped(&mut self, shipped: Shipped) {
        let Shipped { item, lease, lane } = shipped;
        tracing::info!(
            "{}: expert {} shipped: checking its change on lane {lane}",
            self.id,
            item.id
        );
        self.take_lease(&item, lease, ItemState::Checking);
        self.remember(&item.id, Some(lane), None, None);
        self.save_leases();
    }
}
