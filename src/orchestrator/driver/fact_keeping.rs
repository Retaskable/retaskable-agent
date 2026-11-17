//! The facts a computer keeps for the site, sent after each good fetch (docs/specs/intent-rows.md section 5.2;
//! `storeLastRelease` and `storeMainPrDraft` in the TS agent, called from `orchestratorSync.ts` after the fetch): the
//! last release (looked up at most every five minutes, written only when it changed) and what Open PR to main would
//! send (kept per ahead count and head of the base branch), then whatever is owed ([`crate::kept_facts`]): a draft's
//! files, the Environments read's answer. Plain code. Nothing is written until the store's own rows for these have
//! been read once (fail closed), and a failure only waits for the next fetch.
use std::path::Path;

use super::{Links, ProjectLoop};
use crate::kept_facts::{self, KeptFacts, StoreFacts, last};
use crate::release_notes::SavedReleases;

impl ProjectLoop {
    pub(super) async fn tell_facts(&mut self, links: &Links, repo: u64, token: &str, main: &str) {
        let facts = kept_facts::shared(self.store.dir());
        facts.attach(self.store.dir());
        let store = StoreFacts {
            client: self.client.clone(),
            repo,
            token: token.to_string(),
            machine: links.machine.clone(),
        };
        facts.seed(&store).await;
        if facts.seeded() {
            self.keep_release_last(&facts, repo, token).await;
            self.keep_main_pr_draft(&facts, main).await;
        }
        self.keep_issue_import(&facts);
        facts.flush(&store).await;
    }

    /// Mirrors how the last issue check ended as the `issue_import` fact, once per change (`keepIssueImport`).
    fn keep_issue_import(&mut self, facts: &KeptFacts) {
        let Some(state) = crate::issues::schedule::Issues::shared().import_state(&self.id) else {
            return;
        };
        if self.issue_import_told.as_ref() != Some(&state) {
            facts.keep_issue_import(state.as_deref());
            self.issue_import_told = Some(state);
        }
    }

    async fn keep_release_last(&mut self, facts: &KeptFacts, repo: u64, token: &str) {
        if !facts.release_due(last::EVERY) {
            return;
        }
        let client = self.client.clone();
        let saved = SavedReleases {
            client: &client,
            repo_id: repo,
            token: Some(token),
        };
        let p = &self.project;
        let dir = Path::new(&p.repo_path);
        // The store's notes can't be read: what was kept stands.
        if let Some(found) = last::find(&saved, Path::new("gh"), dir, &p.github_repo, &p.base_branch).await {
            facts.keep_release_last(found.as_ref());
        }
    }

    async fn keep_main_pr_draft(&mut self, facts: &KeptFacts, main: &str) {
        let p = &self.project;
        let base = p.base_branch.clone();
        // A project on the default branch has no team branch: nothing is ahead of main.
        if base == main {
            facts.keep_main_pr_draft(None);
            return;
        }
        let (dir, main_owned) = (std::path::PathBuf::from(&p.repo_path), main.to_string());
        let counted = {
            let (dir, base, main) = (dir.clone(), base.clone(), main_owned.clone());
            tokio::task::spawn_blocking(move || crate::team_branch::ahead_behind(&dir, &base, &main)).await
        };
        // A count git can't give keeps the draft as it was.
        let Ok(Ok((ahead, _))) = counted else { return };
        if ahead == 0 {
            facts.keep_main_pr_draft(None);
            return;
        }
        let sha = super::report::git(&dir, &["rev-parse", &format!("origin/{base}")])
            .await
            .filter(|s| !s.is_empty());
        if facts.held_main_pr() == Some((u64::from(ahead), sha.clone())) {
            return;
        }
        let repo = p.github_repo.clone();
        let drafted = {
            let (dir, base, main) = (dir.clone(), base.clone(), main_owned.clone());
            tokio::task::spawn_blocking(move || {
                crate::team_branch::main_pr::draft_main_pr(Path::new("gh"), &dir, &repo, &base, &main)
            })
            .await
        };
        // Not kept: the site asks a computer for it as before.
        match drafted {
            Ok(Ok(d)) => facts.keep_main_pr_draft(Some((&d.title, &d.body, u64::from(ahead), sha.as_deref()))),
            Ok(Err(e)) => tracing::debug!("{}: no main PR draft kept: {e:#}", self.id),
            Err(e) => tracing::debug!("{}: no main PR draft kept: {e}", self.id),
        }
    }
}
