//! Starts each project's rollout loop (docs/specs/rollout-on-server.md section 3, step 1).
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::deps::Live;
use super::{Rollout, uses_store};
use crate::backlog::Client;
use crate::config::ProjectConfig;
use crate::link::TokenStore;
use crate::view::{ItemCommands, PauseState, StorePause, StorePauseState};

pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub backlog_url: &'a str,
    pub keychain: &'a Arc<dyn TokenStore>,
    pub paused: &'a PauseState,
    pub store_paused: &'a StorePauseState,
    pub item_commands: &'a ItemCommands,
    pub machine: String,
}

/// One task per project that rolls out through the store. A computer that may not release the project (it is
/// paused, or its commands wait for approval) sends no report and does not poll; the gate is re-read each round.
pub fn spawn_all(args: SpawnArgs) {
    for project in args.projects {
        let id = project.id_or_derived();
        if project.github_repo.is_empty() || project.repo_path.is_empty() {
            continue;
        }
        if let Err(why) = uses_store(project) {
            tracing::info!("{id}: rollout: {why}");
            continue;
        }
        let (paused, store_paused, commands) = (
            args.paused.clone(),
            args.store_paused.clone(),
            args.item_commands.clone(),
        );
        let file_project = project.clone();
        let live = Live {
            project: project.clone(),
            project_dir: args.dir.join("projects").join(&id),
            machine: args.machine.clone(),
            backlog_url: args.backlog_url.to_string(),
            keychain: args.keychain.clone(),
            client: Client::new(args.backlog_url),
            repo_id: Mutex::new(None),
        };
        let sink_id = id.clone();
        let Ok(rollout) = Rollout::new(
            live,
            project,
            Box::new(move |line| tracing::info!("{sink_id}: rollout: {line}")),
        ) else {
            continue;
        };
        tokio::spawn(async move {
            let mut rollout = rollout;
            loop {
                // This computer's own gate: not paused, and the commands approved.
                let allowed = commands.allowed_config(&id, &file_project);
                let may = paused.reason(&id).is_none()
                    && !matches!(store_paused.get(&id), StorePause::Paused(_))
                    && uses_store(&allowed).is_ok();
                if may {
                    rollout.apply(&allowed);
                    let wait = rollout.cycle().await;
                    tokio::time::sleep(wait).await;
                } else {
                    tokio::time::sleep(super::WAIT).await;
                }
            }
        });
    }
}
