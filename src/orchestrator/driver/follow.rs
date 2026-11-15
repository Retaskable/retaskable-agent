//! Telling the chat how an approved draft moves on (docs/specs/agent/06.6-ask-claude.md, "Retaskable's own
//! messages"; chat-follow-up.md section 4). Plain code, worked out from the items the pass read and posted once each:
//! a message the conversation already has (`ask::follow_up::post_once`) isn't posted again, so a restart repeats
//! nothing. A draft is followed once it has a message of Retaskable's own (written, or approved), so a draft that
//! posted nothing (a chore's) stays silent.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::report::{contains, git, served_sha, version_url};
use super::{Links, ProjectLoop};
use crate::ask::follow_up;
use crate::backlog::{Item, ItemStatus};

/// Staging is looked at this often, and every `LATE_EVERY` once it is late.
const LOOK_EVERY: Duration = Duration::from_secs(30);
const LATE_AFTER: Duration = Duration::from_secs(20 * 60);
const LATE_EVERY: Duration = Duration::from_secs(5 * 60);
const GIVE_UP: Duration = Duration::from_secs(24 * 60 * 60);

/// A draft's wait for staging: the base head its last merge is in, since when, and the last look.
pub(super) struct Wait {
    sha: String,
    since: Instant,
    looked: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Follow {
    staging: BTreeMap<String, Wait>,
}

impl Follow {
    /// Makes the next look at staging due now.
    #[cfg(test)]
    pub(super) fn expedite(&mut self) {
        self.staging.values_mut().for_each(|w| w.looked = None);
    }
}

fn done(status: ItemStatus) -> bool {
    matches!(
        status,
        ItemStatus::OnBranch | ItemStatus::OnMain | ItemStatus::InProduction
    )
}

/// What an item's state says, in order.
fn item_messages(item: &Item) -> Vec<String> {
    let title = &item.title;
    let mut out = Vec::new();
    match item.status {
        ItemStatus::Held | ItemStatus::Skipped => {}
        ItemStatus::Todo if item.slice == 0 => {}
        _ => out.push(follow_up::started(title)),
    }
    match item.status {
        ItemStatus::NeedsPerson => out.push(follow_up::needs_you(title)),
        ItemStatus::Todo if item.slice > 0 => out.push(follow_up::merged_in_part(title)),
        status if done(status) => out.push(follow_up::built(title)),
        _ => {}
    }
    out
}

impl ProjectLoop {
    /// Posts what the items of drafts being followed did since the last pass.
    pub(super) async fn follow_drafts(&mut self, items: &[Item], links: &Links) {
        let mut drafts: BTreeMap<&str, Vec<&Item>> = BTreeMap::new();
        for item in items {
            if let Some(draft) = &item.draft_id {
                drafts.entry(draft).or_default().push(item);
            }
        }
        let Some(repo) = self.repo else { return };
        if drafts.is_empty() {
            return;
        }
        let Ok(Some(token)) = links.keychain.get(&links.backlog_url) else {
            return;
        };
        let asks = match self.client.asks_list(repo, Some(&token)).await {
            Ok(asks) => asks,
            Err(e) => return tracing::warn!("{}: reading the chat for a draft's follow-up: {e}", self.id),
        };
        for (draft, items) in drafts {
            let mine = |a: &&crate::backlog::asks::Ask| {
                a.from.as_deref() == Some("retaskable") && a.draft_id.as_deref() == Some(draft)
            };
            let ended = |a: &crate::backlog::asks::Ask| {
                a.text == follow_up::NO_STAGING || a.text.starts_with("It's on staging")
            };
            if !asks.iter().any(|a| mine(&a)) || asks.iter().filter(mine).any(ended) {
                continue;
            }
            let mut texts: Vec<String> = items.iter().flat_map(|i| item_messages(i)).collect();
            texts.extend(
                items
                    .iter()
                    .filter(|i| i.status == ItemStatus::Skipped)
                    .map(|i| follow_up::skipped(&i.title)),
            );
            if let Some(text) = self.staging_message(draft, &items).await {
                texts.push(text);
            }
            for text in texts {
                if let Err(e) = follow_up::post_once(&self.client, repo, &token, draft, None, &text).await {
                    tracing::warn!("{}: {draft}'s chat message didn't reach the store: {e}", self.id);
                    break;
                }
            }
        }
    }

    /// The last message, once every item the draft added is built or skipped (and one was built): the staging link
    /// when staging serves the merge, that it hasn't yet, or that there is no staging link.
    async fn staging_message(&mut self, draft: &str, items: &[&Item]) -> Option<String> {
        let all_in = items.iter().all(|i| done(i.status) || i.status == ItemStatus::Skipped);
        if !all_in || !items.iter().any(|i| done(i.status)) {
            return None;
        }
        let base = self.project.base_branch.clone();
        let staging = (self.project.deploy.staging.as_ref())
            .filter(|s| s.branch.trim() == base && version_url(s).is_some())
            .cloned();
        let Some(staging) = staging else {
            return Some(follow_up::NO_STAGING.into());
        };
        let repo_path = std::path::PathBuf::from(&self.project.repo_path);
        if !self.follow.staging.contains_key(draft) {
            let at = format!("origin/{base}^{{commit}}");
            let sha = git(&repo_path, &["rev-parse", "--verify", "--quiet", &at]).await?;
            let wait = Wait {
                sha,
                since: Instant::now(),
                looked: None,
            };
            self.follow.staging.insert(draft.to_string(), wait);
        }
        let wait = self.follow.staging.get_mut(draft)?;
        let late = wait.since.elapsed() >= LATE_AFTER;
        if wait.since.elapsed() >= GIVE_UP {
            return None;
        }
        let every = if late { LATE_EVERY } else { LOOK_EVERY };
        if wait.looked.is_some_and(|at| at.elapsed() < every) {
            return None;
        }
        wait.looked = Some(Instant::now());
        let sha = wait.sha.clone();
        if let Some(served) = served_sha(&repo_path, &version_url(&staging)?).await
            && (served == sha || contains(&repo_path, &sha, &served).await)
        {
            return Some(follow_up::on_staging(staging.url.as_deref()?.trim()));
        }
        late.then(|| follow_up::STAGING_LATE.to_string())
    }
}
