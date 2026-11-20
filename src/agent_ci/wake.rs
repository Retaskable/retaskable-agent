//! The CI runner's sockets (docs/specs/agent-ci.md section 3.11, store-push-now.md section 5): one to each repo's store
//! object, opened the way the Coding Agent opens its own (`backlog_socket`), which only listens. The store sends a socket of
//! role `ci` `ci_wake` and nothing else; it wakes the loop's wait so the next poll comes at once instead of after
//! `ciPollSeconds`. A push is a hint: the poll still reads what the store says, and a missed one is only the next poll.
use std::sync::Arc;

use tokio::sync::Notify;

use super::store::StoreOps;
use crate::backlog_socket::{Deps, Handle, Push, RepoId, Unknown, start};

/// Opens a socket for each of `repos` (`owner/repo`). They end when the handles are stopped or dropped.
pub fn listen(store: &Arc<StoreOps>, repos: &[String], wake: &Arc<Notify>) -> Vec<Handle> {
    repos
        .iter()
        .map(|repo| {
            let (ids, name) = (store.clone(), repo.clone());
            let repo_id: RepoId = Arc::new(move || {
                let (ids, name) = (ids.clone(), name.clone());
                Box::pin(async move {
                    ids.repo_id(&name)
                        .await
                        .map_err(|why| Unknown { why, retry_after: None })
                })
            });
            start(Deps::new(
                repo.clone(),
                Arc::new(store.tokens().clone()),
                repo_id,
                pushed(wake.clone()),
            ))
        })
        .collect()
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
