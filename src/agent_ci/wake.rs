//! The CI runner's sockets (docs/specs/agent-ci.md section 3.11, store-push-now.md section 5): one to each repo's store
//! object, kept as the settings' repos change, opened the way the Coding Agent opens its own (`backlog_socket`), which only listens. The store sends a socket of
//! role `ci` `ci_wake` and nothing else; it wakes the loop's wait so the next poll comes at once instead of after
//! `ciPollSeconds`. A push is a hint: the poll still reads what the store says, and a missed one is only the next poll.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio::sync::Notify;

use super::store::StoreOps;
use crate::backlog_socket::{Deps, Handle, Push, RepoId, Unknown, start};

/// The sockets the runner holds open, one to each repo's store object (the store's objects are per repo, so a socket
/// can't be shared). [`Sockets::sync`] makes them the repos of the settings as they are now: the settings are read
/// again on every poll, so a repo added later gets its socket (and its `ci_wake` and its presence there) and one
/// taken out has its socket closed, where they were opened once at the start and a repo added after never had one.
pub struct Sockets {
    open: BTreeMap<String, Handle>,
    spawn: Box<dyn Fn(&str) -> Handle + Send + Sync>,
}

impl Sockets {
    /// Sockets opened the real way: with the link's own tokens, `wake` woken by what comes down them.
    pub fn new(store: &Arc<StoreOps>, wake: &Arc<Notify>) -> Self {
        let (store, wake) = (store.clone(), wake.clone());
        Self::with(move |repo| {
            let (ids, name) = (store.clone(), repo.to_string());
            let repo_id: RepoId = Arc::new(move || {
                let (ids, name) = (ids.clone(), name.clone());
                Box::pin(async move {
                    ids.repo_id(&name)
                        .await
                        .map_err(|why| Unknown { why, retry_after: None })
                })
            });
            start(Deps::new(
                repo.to_string(),
                Arc::new(store.tokens().clone()),
                repo_id,
                pushed(wake.clone()),
            ))
        })
    }

    /// Sockets opened by `spawn` (tests).
    pub fn with(spawn: impl Fn(&str) -> Handle + Send + Sync + 'static) -> Self {
        Self {
            open: BTreeMap::new(),
            spawn: Box::new(spawn),
        }
    }

    /// Opens a socket for each of `repos` (`owner/repo`) that has none, and closes the ones of repos no longer listed
    /// (a dropped handle closes its socket).
    pub fn sync<'a>(&mut self, repos: impl IntoIterator<Item = &'a str>) {
        let wanted: BTreeSet<&str> = repos.into_iter().collect();
        self.open.retain(|repo, _| wanted.contains(repo.as_str()));
        for repo in wanted {
            if !self.open.contains_key(repo) {
                self.open.insert(repo.to_string(), (self.spawn)(repo));
            }
        }
    }

    /// The repos with a socket, in order.
    pub fn repos(&self) -> Vec<&str> {
        self.open.keys().map(String::as_str).collect()
    }
}

/// What a push does: `ci_wake` wakes the loop, and so does a socket that opened again (it may have missed one). The rest is
/// not for this socket.
fn pushed(wake: Arc<Notify>) -> Arc<dyn Fn(Push) + Send + Sync> {
    Arc::new(move |push| {
        if matches!(push, Push::CiWake | Push::Reconnected) {
            wake.notify_one();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog_socket::parse;

    fn idle_socket(repo: &str) -> Handle {
        struct Never;
        impl crate::backlog_socket::Tokens for Never {
            fn access_token(
                &self,
                _: Option<String>,
            ) -> futures_util::future::BoxFuture<'static, crate::link::TokenAnswer> {
                Box::pin(std::future::pending())
            }
        }
        let repo_id: RepoId = Arc::new(|| Box::pin(std::future::pending()));
        start(Deps::new(repo.into(), Arc::new(Never), repo_id, Arc::new(|_| {})))
    }

    #[tokio::test]
    async fn a_repo_added_to_the_settings_gets_a_socket_and_one_taken_out_loses_its_own() {
        let opened = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = opened.clone();
        let mut sockets = Sockets::with(move |repo| {
            log.lock().unwrap().push(repo.to_string());
            idle_socket(repo)
        });
        sockets.sync(["o/a", "o/b"]);
        assert_eq!(sockets.repos(), ["o/a", "o/b"]);
        // The settings were read again with more repos: only the new ones are opened, the old ones are kept.
        sockets.sync(["o/a", "o/b", "o/c", "o/d"]);
        assert_eq!(sockets.repos(), ["o/a", "o/b", "o/c", "o/d"]);
        sockets.sync(["o/a", "o/b", "o/c", "o/d"]);
        assert_eq!(
            *opened.lock().unwrap(),
            ["o/a", "o/b", "o/c", "o/d"],
            "one socket each, never two"
        );
        sockets.sync(["o/b"]);
        assert_eq!(sockets.repos(), ["o/b"]);
        sockets.sync([]);
        assert!(sockets.repos().is_empty());
    }

    #[tokio::test]
    async fn only_ci_wake_and_a_reopened_socket_wake_the_loop() {
        assert_eq!(parse(r#"{"type":"ci_wake"}"#), Some(Push::CiWake));
        assert_eq!(parse(r#"{"type":"ci_wake","repo":"o/r"}"#), Some(Push::CiWake));
        let wake = Arc::new(Notify::new());
        let push = pushed(wake.clone());
        let woken = |wake: &Arc<Notify>| {
            let w = wake.clone();
            async move {
                tokio::time::timeout(std::time::Duration::from_millis(50), w.notified())
                    .await
                    .is_ok()
            }
        };
        push(Push::WorkAvailable);
        push(Push::PausedChanged);
        assert!(!woken(&wake).await, "another push is not for the runner");
        push(Push::CiWake);
        assert!(woken(&wake).await);
        push(Push::Reconnected);
        assert!(woken(&wake).await);
    }

    #[tokio::test]
    async fn a_wake_ends_the_wait_and_many_during_a_poll_make_one_more() {
        let wake = Arc::new(Notify::new());
        let ci = super::super::CiLoop::new(Default::default()).with_wake(wake.clone());
        // Two wakes while a poll runs: one more poll at once, then the wait is a wait again.
        wake.notify_one();
        wake.notify_one();
        let start = std::time::Instant::now();
        ci.nap(std::time::Duration::from_secs(30)).await;
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            ci.nap(std::time::Duration::from_secs(30)),
        )
        .await;
        assert!(waited.is_err(), "the second wake was not kept");
        // No wake source: it just sleeps.
        let plain = super::super::CiLoop::new(Default::default());
        plain.nap(std::time::Duration::from_millis(1)).await;
    }
}
