//! What the minute's fetch shows of the base branch, kept for `health`'s `main` (pools-on-server.md section 15.1; `afterFetch`
//! and `mainFacts` in the TypeScript agent's `mainLook.ts`): the branch's head, the workflows' tree, and how far behind the
//! default branch it is. Facts only: this computer decides nothing about main being red or behind, and looks at it only when the
//! store sends a `main` job (`crate::main_look`).
use std::path::PathBuf;

use super::{Links, ProjectLoop};
use crate::item_lifecycle::ItemState;
use crate::main_look::{Behind, Rebase, Seen, head_of};
use crate::main_sync::behind_count;

impl ProjectLoop {
    /// After a good fetch: finishes a restart a ship left undone, then keeps what the fetch showed of the base branch.
    /// `default` is the repo's default branch, when `gh` could say it.
    pub(super) async fn see_main(&mut self, links: &Links, repo: u64, token: &str, default: Option<&str>) {
        let base = self.project.base_branch.clone();
        if base.is_empty() || self.project.repo_path.trim().is_empty() {
            return;
        }
        let team = default.filter(|m| *m != base).map(str::to_string);
        // The counts below are from after a restart moved the base, so a pass that finished one counts again.
        if let Some(main) = &team {
            self.finish_restart(repo, Some(token), &links.machine, main).await;
        }
        let (dir, b) = (PathBuf::from(&self.project.repo_path), base.clone());
        let rebases = crate::main_rebase::uses_rebase(&self.project);
        let seen = tokio::task::spawn_blocking(move || {
            let sha = head_of(&dir, &b)?;
            let tree = crate::ci_coverage::workflows_tree(&dir, &format!("origin/{b}"));
            // How far the default branch is ahead: as `behind` for a branch the store syncs by a pull request, as `rebase` (with
            // both heads) for the integration-branch pattern, which the store catches up by a `rebase` job.
            let (mut behind, mut rebase) = (None, None);
            if let Some(main) = team {
                let count = behind_count(&dir, &b, &main).ok();
                let head = head_of(&dir, &main);
                if let (Some(count), Some(head)) = (count, head) {
                    if rebases {
                        rebase = Some(Rebase {
                            branch: main,
                            head,
                            base: sha.clone(),
                            count,
                            waiting: Vec::new(),
                        });
                    } else {
                        behind = Some(Behind {
                            branch: main,
                            head,
                            count,
                        });
                    }
                }
            }
            Some(Seen {
                branch: b,
                sha,
                tree,
                behind,
                rebase,
            })
        })
        .await
        .ok()
        .flatten();
        let Some(seen) = seen else { return };
        // `.agent-ci/` on the base branch makes CI required and only when ready, unless the settings say otherwise.
        self.agent_ci = crate::ci_coverage::agent_ci_in(seen.tree.as_deref());
        self.project.agent_ci = self.agent_ci;
        // A note that a fix found nothing to fix is about one commit.
        if self.main_look.head().is_some_and(|head| head != seen.sha) {
            self.live.set_fix_main(&self.id, None);
        }
        self.main_look.see(seen);
    }

    /// The items and drafts whose branch was cut from the base branch and hasn't gone in yet (`waitingOnBase`): the rebase
    /// rewrites that branch's history, so the store holds it back while any exists. An item asking a question or stuck before it
    /// had a branch doesn't hold it up, and a main sync's own item isn't one. Read live, each time it is used.
    pub(super) fn waiting_on_base(&self) -> Vec<String> {
        let open = |state: ItemState| {
            matches!(
                state,
                ItemState::Implementing
                    | ItemState::NeedsClarification
                    | ItemState::AwaitingCi
                    | ItemState::Checking
                    | ItemState::Reviewing
                    | ItemState::AwaitingPlaytest
                    | ItemState::Merging
                    | ItemState::AwaitingMerge
                    | ItemState::NeedsHuman
            )
        };
        let has_branch = |id: &str| {
            self.records
                .get(id)
                .is_some_and(|r| r.branch.is_some() || r.flow.pr_url.is_some())
        };
        let is_sync = |id: &str| self.chores.get(id).is_some_and(|w| w.kind == "sync_main");
        let mut waiting: Vec<String> = (self.runs.iter())
            .filter(|(id, r)| open(r.state) && (r.state == ItemState::Implementing || has_branch(id)) && !is_sync(id))
            .map(|(id, _)| id.clone())
            .collect();
        // Pieces taken from the store's own queue run beside these (`runs` mode): each live one holds its item's branch.
        waiting.extend(self.run_loop.runs().live().iter().map(|r| r.piece.clone()));
        waiting.extend(self.draft.iter().map(|d| d.id.clone()));
        waiting.sort();
        waiting.dedup();
        waiting
    }
}
