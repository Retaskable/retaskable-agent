//! Shipping an expert session in `runs` mode (docs/specs/expert-chat/14-ship-as-a-piece-2026-11-16.md): no item loop
//! holds an item here, so the store takes the session's open change as the item's piece 1, already `in_ci`
//! (`expert_ship`), and runs on any computer watch, fix and merge it. This computer lets the session and its lane go.
//!
//! The change is pushed as the same `retaskable/<id>-<slug>-s0` branch as in `claim` mode (the piece's `branch` is
//! what its runs check out as given, `piece_runs.rs`, and the store's CI watcher skips a `retaskable/` branch an item
//! has). No `in_review` / `expert_shipped` marker goes first: nothing here keeps the item, and the store holds the
//! piece, not this computer.
//!
//! Between the change opening and the store taking it a restart would forget the session (it is in memory only), so
//! the call is listed in the state file (`ShipOwed`) before it is sent and sent again every pass, also after a restart,
//! until the store answers. `expert_ship` is idempotent for a lease and a change, so a lost reply is just an answer
//! here too. Only a definite refusal drops the entry. In a session's own command the refusal is the answer, in words,
//! and the session stays as it was (its change is open); after a restart there is no session and nobody to answer, so
//! it is logged as an error.
use super::*;
use crate::backlog::BacklogError;
use crate::backlog::expert::ShipOwed;
use crate::store::Store;

/// Refusals sending the same call again can't change: nothing was changed by them.
fn definite(e: &BacklogError) -> bool {
    matches!(
        e.code(),
        Some("forbidden" | "conflict" | "stale_lease" | "not_found" | "bad_request")
    )
}

/// Commits what is uncommitted, pushes the item's branch and opens its change (or finds the one open), from the
/// session's lane (a parked session takes one again). No lane free is an answer. The result is what the store is
/// owed; the session is not touched but for the lane it took, which it keeps (and the lane is on the item's branch).
pub(super) async fn open(cx: &Cx<'_>, pool: &mut LanePool, item: &str, s: &mut Session) -> Result<ShipOwed, String> {
    let (lane, took) = match s.lane {
        Some(lane) => (lane, false),
        None => match pool.claim(&cx.want(item)) {
            Some(lane) => (lane, true),
            None => return Err("No lane is free to ship from; try again when one is.".into()),
        },
    };
    let target = crate::orchestrator::session::branch_name(&s.item, 0);
    let job = ShipJob {
        project: cx.project.clone(),
        lane,
        branch: s.branch.clone(),
        target: target.clone(),
        prepare: took || !s.prepared,
        title: format!("{item}: {}", s.title),
        body: super::ship::body(&s.request, &s.started_by),
    };
    match cx.host.ship(job).await {
        Ok(opened) => {
            s.lane = Some(lane);
            s.prepared = true;
            Ok(ShipOwed {
                lease: s.lease.clone(),
                branch: target,
                pr_url: opened.change.url,
                pr_number: opened.change.number,
                head: opened.head,
                title: crate::text::truncate(s.title.trim(), 200),
            })
        }
        Err(why) => {
            if took {
                pool.release(lane, &cx.holder(item));
            }
            Err(format!("Couldn't push {} or open its pull request: {why}", s.branch))
        }
    }
}

/// Sends the item's owed call. Taken (or taken before): the session is let go, with its lane and its queue, and the
/// entry is no longer owed. Refused: the entry goes and the refusal is the answer, in words, the session as it was.
/// Not answered (the store is out of reach): the session is let go all the same, since the call is kept and sent
/// again, and the person's change is open.
pub(super) async fn settle(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool, item: &str) -> Result<(), String> {
    let Some(owed) = ex.owed.get(item).cloned() else {
        return Ok(());
    };
    match cx.client.expert_ship(cx.repo, Some(cx.token), &owed.change(item)).await {
        Ok(piece) => {
            tracing::info!("{}: expert {item} shipped as piece {}", cx.id, piece.piece);
            ex.owed.remove(item);
            let_go(cx, ex, pool, item).await;
            cx.queue.clear(item).await;
            Ok(())
        }
        Err(e) if definite(&e) => {
            ex.owed.remove(item);
            Err(format!(
                "Its pull request {} is open, but the backlog store wouldn't take it as the item's piece, so nothing carries it on yet: {e}. \
                 The session is as it was; the change stays open.",
                owed.pr_url
            ))
        }
        Err(e) => {
            tracing::warn!(
                "{}: expert {item}: {} is open, and the store will hear of it as a piece when it is back: {e}",
                cx.id,
                owed.pr_url
            );
            let_go(cx, ex, pool, item).await;
            Ok(())
        }
    }
}

/// The session is over: it is forgotten and its lane freed.
async fn let_go(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool, item: &str) {
    if let Some(s) = ex.sessions.remove(item)
        && let Some(lane) = s.lane
    {
        pool.release(lane, &cx.holder(item));
    }
}

/// Sends again every call still owed from an earlier pass or an earlier run of the agent (no session holds those).
pub(super) async fn retry(cx: &Cx<'_>, ex: &mut Experts, pool: &mut LanePool) {
    for item in ex.owed.keys().cloned().collect::<Vec<_>>() {
        let url = ex.owed[&item].pr_url.clone();
        if let Err(why) = settle(cx, ex, pool, &item).await {
            tracing::error!("{}: expert {item}: {why} ({url} was left open)", cx.id);
        }
    }
}

/// Saves the owed calls when they differ from `saved`.
pub(super) fn save(store: &mut Store, ex: &Experts, saved: &mut BTreeMap<String, ShipOwed>) {
    if *saved == ex.owed {
        return;
    }
    store.set_ship_owed(&ex.owed);
    if let Err(e) = store.save() {
        tracing::warn!("saving the expert changes owed to the store failed: {e:#}");
    }
    saved.clone_from(&ex.owed);
}
