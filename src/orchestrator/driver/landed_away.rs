//! An item whose change merged while it was away (`landedWhileAway` in the TypeScript agent's `merge.ts`). Before a
//! session is launched for an item that has a change, the host is asked whether that change merged meanwhile (parked,
//! waiting on a person, cut by a restart, or merged on the host by a person or another tool); if so the item is
//! recorded as merged, its lane is freed and the store hears it landed, as when its own merge step ends, instead of a
//! session picking up a branch the host has merged and deleted.
use super::Links;
use super::project::{ProjectLoop, SessionStarter};
use crate::drafts::land::pr_number;
use crate::item_lifecycle::{ItemState, MergeOutcome};
use crate::lane_pool::LanePool;
use crate::orchestrator::steps::MergeEnd;

impl ProjectLoop {
    /// True when `item_id`'s change is merged, which has been taken in; the caller launches nothing.
    pub(super) async fn landed_while_away(
        &mut self,
        pool: &mut LanePool,
        item_id: &str,
        starter: &dyn SessionStarter,
        links: &Links,
    ) -> bool {
        let Some(item) = self.items.get(item_id) else {
            return false;
        };
        let pr = (self.records.get(item_id))
            .and_then(|r| r.flow.pr_number)
            .or_else(|| item.pr_url.as_deref().and_then(pr_number));
        let Some(pr) = pr else { return false };
        let branch = crate::orchestrator::session::branch_name(item, self.attempt_of(item_id));
        let repo = std::path::PathBuf::from(&self.project.repo_path);
        let Some(commit) = starter.landed(&repo, pr, &branch).await else {
            return false;
        };
        let Some(run) = self.runs.get_mut(item_id) else {
            return false;
        };
        let from = run.state;
        run.merge_finished(MergeOutcome::Merged, &self.project);
        debug_assert_eq!(run.state, ItemState::Merged);
        // The base branch moved on the host: the next lane fetches it rather than trusting a recent fetch.
        crate::lanes::fetch_is_stale(&repo);
        tracing::info!("{}: {item_id}'s pull request #{pr} merged while it was away", self.id);
        self.measure_away(item_id);
        self.moved(pool, item_id, from, "its change merged while it was away", links)
            .await;
        self.merge_ended(item_id, MergeEnd::Merged { commit }, links).await;
        true
    }
}
