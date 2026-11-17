//! What the minute's fetch shows of the base branch, kept for `health`'s `main` (pools-on-server.md section 15.1; `afterFetch`
//! and `mainFacts` in the TypeScript agent's `mainLook.ts`): the branch's head, the workflows' tree, and how far behind the
//! default branch it is. Facts only: this computer decides nothing about main being red or behind, and looks at it only when the
//! store sends a `main` job (`crate::main_look`).
use std::path::PathBuf;

use super::{Links, ProjectLoop};
use crate::main_look::{Behind, Seen, head_of};
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
        let seen = tokio::task::spawn_blocking(move || {
            let sha = head_of(&dir, &b)?;
            let tree = crate::ci_coverage::workflows_tree(&dir, &format!("origin/{b}"));
            let behind = team.and_then(|main| {
                let count = behind_count(&dir, &b, &main).ok()?;
                let head = head_of(&dir, &main)?;
                Some(Behind {
                    branch: main,
                    head,
                    count,
                })
            });
            Some(Seen {
                branch: b,
                sha,
                tree,
                behind,
            })
        })
        .await
        .ok()
        .flatten();
        let Some(seen) = seen else { return };
        // A note that a fix found nothing to fix is about one commit.
        if self.main_look.head().is_some_and(|head| head != seen.sha) {
            self.live.set_fix_main(&self.id, None);
        }
        self.main_look.see(seen);
    }
}
