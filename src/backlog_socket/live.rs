//! The real sockets: one per project on GitHub, on this computer's one access token for the store, each waking what
//! its pushes are for (`wake`). Started with the agent whether or not it has claimed anything (backlogSocket.ts is
//! started per hosted project, always): an agent that isn't linked looks again once a minute and no wake is lost for
//! it, since a wake only ends a wait.
use std::sync::Arc;

use super::wake::wake;
use super::{Deps, Handle, Push, start};
use crate::config::ProjectConfig;
use crate::link::{BACKLOG_TARGET, DeviceTokens, TokenStore};
use crate::work_signal::WorkSignals;

pub struct SpawnArgs<'a> {
    pub projects: &'a [ProjectConfig],
    pub backlog_url: &'a str,
    pub keychain: &'a Arc<dyn TokenStore>,
    pub http: &'a reqwest::Client,
    pub wakes: &'a WorkSignals,
}

/// Starts each project's socket; they close when the handles are dropped or stopped. A project with no `githubRepo`
/// has no backlog id to name and gets none (the orchestrator skips it too).
pub fn spawn_all(args: SpawnArgs) -> Vec<Handle> {
    let tokens = Arc::new(DeviceTokens::new(
        args.backlog_url.to_string(),
        BACKLOG_TARGET,
        args.keychain.clone(),
        args.http.clone(),
    ));
    let mut sockets = Vec::new();
    for project in args.projects {
        if project.github_repo.is_empty() {
            continue;
        }
        let id = project.id_or_derived();
        let github_repo = project.github_repo.clone();
        // The repo's id is asked of `gh` until it answers, then kept.
        let known = Arc::new(tokio::sync::Mutex::new(None::<u64>));
        let repo_id = Arc::new(move || {
            let (known, github_repo) = (known.clone(), github_repo.clone());
            Box::pin(async move {
                let mut known = known.lock().await;
                if known.is_none() {
                    *known = crate::repo_id::repo_id(&github_repo).await;
                }
                *known
            }) as futures_util::future::BoxFuture<'static, Option<u64>>
        });
        let wakes = args.wakes.clone();
        let pushed_for = id.clone();
        let pushed = Arc::new(move |push: Push| wake(&wakes, &pushed_for, &push));
        sockets.push(start(Deps::new(id, Arc::new(tokens.clone()), repo_id, pushed)));
    }
    sockets
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project with no GitHub repo has no backlog to name; one with a repo gets a socket, which with no link to the
    /// store asks for nothing and touches no network.
    #[tokio::test]
    async fn one_socket_per_project_on_github_and_none_without_a_link_to_talk_to() {
        let dir = tempfile::tempdir().unwrap();
        let keys: Arc<dyn TokenStore> = Arc::new(crate::link::FileStore::new(dir.path().join("keys.json"), "test"));
        let named = |id: &str, repo: &str| ProjectConfig {
            id: Some(id.into()),
            github_repo: repo.into(),
            ..Default::default()
        };
        let projects = [named("a", "o/a"), named("b", ""), named("c", "o/c")];
        let http = reqwest::Client::new();
        let wakes = WorkSignals::default();
        let sockets = spawn_all(SpawnArgs {
            projects: &projects,
            backlog_url: "http://127.0.0.1:1",
            keychain: &keys,
            http: &http,
            wakes: &wakes,
        });
        assert_eq!(sockets.len(), 2);
        for s in sockets {
            s.stop().await;
        }
    }
}
