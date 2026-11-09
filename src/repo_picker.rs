//! The repo picker's cache, ported from `GithubPicker`'s `listOwners`/`listRepos` caching
//! (`agent/src/github.ts`). Owner and per-owner repo lists are kept for 60 seconds (the spec's
//! figure); `fresh` fetches again. A failed fetch is never cached.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::repo::{self, RepoSummary};

/// How long a fetched list is kept.
pub const CACHE_TTL: Duration = Duration::from_secs(60);

type OwnersFetch = Box<dyn Fn() -> Result<Vec<String>> + Send + Sync>;
type ReposFetch = Box<dyn Fn(&str) -> Result<Vec<RepoSummary>> + Send + Sync>;
type Clock = Box<dyn Fn() -> Instant + Send + Sync>;

#[derive(Default)]
struct Cache {
    owners: Option<(Instant, Vec<String>)>,
    /// Keyed by the lower-cased owner.
    repos: HashMap<String, (Instant, Vec<RepoSummary>)>,
}

pub struct RepoPicker {
    fetch_owners: OwnersFetch,
    fetch_repos: ReposFetch,
    clock: Clock,
    cache: Mutex<Cache>,
}

impl RepoPicker {
    /// The real `gh` fetchers and `Instant::now`.
    pub fn new() -> Self {
        Self::with(
            Box::new(repo::list_owners),
            Box::new(repo::list_repos),
            Box::new(Instant::now),
        )
    }

    pub fn with(fetch_owners: OwnersFetch, fetch_repos: ReposFetch, clock: Clock) -> Self {
        Self {
            fetch_owners,
            fetch_repos,
            clock,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// The person's owners; `fresh` fetches again and clears every cached repo list.
    pub fn list_owners(&self, fresh: bool) -> Result<Vec<String>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.clock)();
        if fresh {
            cache.owners = None;
            cache.repos.clear();
        } else if let Some((at, owners)) = &cache.owners
            && now.saturating_duration_since(*at) < CACHE_TTL
        {
            return Ok(owners.clone());
        }
        let owners = (self.fetch_owners)()?;
        cache.owners = Some((now, owners.clone()));
        Ok(owners)
    }

    /// `owner`'s repos, cached per owner (case-insensitively).
    pub fn list_repos(&self, owner: &str, fresh: bool) -> Result<Vec<RepoSummary>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let now = (self.clock)();
        let key = owner.to_lowercase();
        if !fresh
            && let Some((at, repos)) = cache.repos.get(&key)
            && now.saturating_duration_since(*at) < CACHE_TTL
        {
            return Ok(repos.clone());
        }
        let repos = (self.fetch_repos)(owner)?;
        cache.repos.insert(key, (now, repos.clone()));
        Ok(repos)
    }
}

impl Default for RepoPicker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Rig {
        picker: RepoPicker,
        owners: Arc<AtomicUsize>,
        repos: Arc<AtomicUsize>,
        offset: Arc<Mutex<Duration>>,
        fail: Arc<Mutex<bool>>,
    }

    fn rig() -> Rig {
        let owners = Arc::new(AtomicUsize::new(0));
        let repos = Arc::new(AtomicUsize::new(0));
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let fail = Arc::new(Mutex::new(false));
        let base = Instant::now();
        let (o, f1) = (owners.clone(), fail.clone());
        let (r, f2) = (repos.clone(), fail.clone());
        let off = offset.clone();
        let picker = RepoPicker::with(
            Box::new(move || {
                o.fetch_add(1, Ordering::SeqCst);
                if *f1.lock().unwrap() {
                    anyhow::bail!("gh down");
                }
                Ok(vec!["me".to_string()])
            }),
            Box::new(move |_| {
                r.fetch_add(1, Ordering::SeqCst);
                if *f2.lock().unwrap() {
                    anyhow::bail!("gh down");
                }
                Ok(vec![])
            }),
            Box::new(move || base + *off.lock().unwrap()),
        );
        Rig {
            picker,
            owners,
            repos,
            offset,
            fail,
        }
    }

    fn advance(rig: &Rig, secs: u64) {
        *rig.offset.lock().unwrap() += Duration::from_secs(secs);
    }

    #[test]
    fn owners_are_cached_for_sixty_seconds() {
        let rig = rig();
        rig.picker.list_owners(false).unwrap();
        advance(&rig, 59);
        rig.picker.list_owners(false).unwrap();
        assert_eq!(rig.owners.load(Ordering::SeqCst), 1);
        advance(&rig, 1);
        rig.picker.list_owners(false).unwrap();
        assert_eq!(rig.owners.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fresh_owners_refetch_and_clear_repo_lists() {
        let rig = rig();
        rig.picker.list_owners(false).unwrap();
        rig.picker.list_repos("me", false).unwrap();
        rig.picker.list_owners(true).unwrap();
        assert_eq!(rig.owners.load(Ordering::SeqCst), 2);
        rig.picker.list_repos("me", false).unwrap();
        assert_eq!(rig.repos.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn repos_cache_per_owner_case_insensitively_and_expire() {
        let rig = rig();
        rig.picker.list_repos("Me", false).unwrap();
        rig.picker.list_repos("ME", false).unwrap();
        assert_eq!(rig.repos.load(Ordering::SeqCst), 1);
        rig.picker.list_repos("other", false).unwrap();
        assert_eq!(rig.repos.load(Ordering::SeqCst), 2);
        rig.picker.list_repos("me", true).unwrap();
        assert_eq!(rig.repos.load(Ordering::SeqCst), 3);
        advance(&rig, 60);
        rig.picker.list_repos("me", false).unwrap();
        assert_eq!(rig.repos.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn errors_are_not_cached() {
        let rig = rig();
        *rig.fail.lock().unwrap() = true;
        assert!(rig.picker.list_owners(false).is_err());
        assert!(rig.picker.list_repos("me", false).is_err());
        *rig.fail.lock().unwrap() = false;
        rig.picker.list_owners(false).unwrap();
        rig.picker.list_repos("me", false).unwrap();
        assert_eq!(rig.owners.load(Ordering::SeqCst), 2);
        assert_eq!(rig.repos.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<RepoPicker>();
    }
}
