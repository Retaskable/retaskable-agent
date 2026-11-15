//! Starts the outbound cloud links (split out of `main.rs`).
use std::path::Path;
use std::sync::Arc;

use retaskable_agent::env::Env;
use retaskable_agent::{backlog, connector, link, view};

use crate::auto_link_backlog;

/// cloud.md section 5 / environments.md section 3.4: starts one outbound link to each cloud,
/// every project on it, and the shared backlog's auto-link beside them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn link_clouds(
    cloud_urls: &[String],
    keychain: &Arc<dyn link::TokenStore>,
    http: &reqwest::Client,
    view: &Arc<view::AgentView>,
    exit: &Arc<view::Exit>,
    dir: &Path,
    version: &str,
    env: &Env,
    links: &link::pending::Links,
) -> Vec<connector::Connector> {
    let connectors: Vec<_> = cloud_urls
        .iter()
        .map(|url| {
            connector::Connector::start(
                url,
                keychain.clone(),
                http.clone(),
                view.clone(),
                connector::Timing::default(),
                exit.clone(),
                dir,
            )
        })
        .collect();
    // installers/03-the-bundle-agent-bundle.md section 3.10: named once, after the connections above
    // start, so the log says whether this build is linked — the start check greps for it, and a
    // release build has no other way to tell.
    let backlog_url = backlog::url(env);
    tracing::info!(
        "{}",
        link::links_line(cloud_urls, &backlog_url, |url| matches!(keychain.get(url), Ok(Some(_))))
    );
    // installers/04-every-agent-on-the-shared-backlog-shared.md: once this computer already holds a
    // retaskable.com link, the shared backlog's device flow starts by itself too, opening its own
    // `/link` page. Fire-and-forget so a slow or unreachable store doesn't delay serving `/api/health`;
    // it isn't tried again this run (a failed or expired code waits for the next start).
    if cloud_urls.iter().any(|url| matches!(keychain.get(url), Ok(Some(_)))) {
        tokio::spawn(auto_link_backlog(
            links.clone(),
            keychain.clone(),
            http.clone(),
            env.clone(),
            version.to_string(),
        ));
    }
    connectors
}
