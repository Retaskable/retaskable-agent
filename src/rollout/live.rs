//! Starts each project's rollout loop (docs/specs/rollout-on-server.md section 3, step 1).
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::deps::Live;
use super::{Rollout, Runners, uses_store};
use crate::backlog::Client;
use crate::backlog::access::AccessTokens;
use crate::config::ProjectConfig;
use crate::view::{ItemCommands, PauseState, StorePause, StorePauseState};

pub struct SpawnArgs<'a> {
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub backlog_url: &'a str,
    pub access: &'a Arc<dyn AccessTokens>,
    pub paused: &'a PauseState,
    pub store_paused: &'a StorePauseState,
    pub item_commands: &'a ItemCommands,
    pub machine: String,
    /// Each project's flag for whether this computer is its runner, for the CI watcher.
    pub runners: &'a Runners,
}

/// One task per project that rolls out through the store. A computer that may not release the project (it is
/// paused, or its commands wait for approval) sends no report and does not poll; the gate is re-read each round.
pub fn spawn_all(args: SpawnArgs) {
    for project in args.projects {
        let id = project.id_or_derived();
        if project.github_repo.is_empty() || project.repo_path.is_empty() {
            continue;
        }
        let project_dir = args.dir.join("projects").join(&id);
        let (paused, store_paused, commands) = (
            args.paused.clone(),
            args.store_paused.clone(),
            args.item_commands.clone(),
        );
        let file_project = project.clone();
        let runner = args.runners.flag(&id);
        let wake = args.runners.wake(&id);
        let live = Live {
            project: project.clone(),
            project_dir: project_dir.clone(),
            machine: args.machine.clone(),
            backlog_url: args.backlog_url.to_string(),
            access: args.access.clone(),
            client: Client::with_access(args.backlog_url, args.access.clone()),
            repo_id: Mutex::new(crate::store::saved_repo_id(&project_dir, &project.github_repo)),
            gh: std::path::PathBuf::from("gh"),
        };
        tokio::spawn(async move {
            let (mut live, mut rollout, mut said) = (Some(live), None::<Rollout<Live>>, String::new());
            loop {
                // This computer's own gate: not paused, and a rollout in the commands as approved now. A project
                // whose commands arrive later (from the store, or a saved settings change) starts then.
                let mut allowed = commands.allowed_config(&id, &file_project);
                // Production only: no staging, and production is the default branch (TS `effectiveDeploy`).
                allowed.deploy = allowed.deploy.effective(&allowed.base_branch);
                let gate = match uses_store(&allowed) {
                    _ if paused.reason(&id).is_some() => Err("paused"),
                    _ if matches!(store_paused.get(&id), StorePause::Paused(_)) => Err("paused in the store"),
                    Ok(_) => Ok(()),
                    Err(why) => Err(why),
                };
                match gate {
                    Ok(()) => {
                        if rollout.is_none() {
                            let sink_id = id.clone();
                            let sink = Box::new(move |line: &str| tracing::info!("{sink_id}: rollout: {line}"));
                            rollout = live
                                .take()
                                .and_then(|l| Rollout::new(l, &allowed, sink).ok())
                                .map(|r| r.with_runner(runner.clone()));
                        }
                        let Some(r) = rollout.as_mut() else { return };
                        r.apply(&allowed);
                        let wait = r.cycle().await;
                        // A fetch that moved main cuts a long wait short: its report goes out at once.
                        tokio::select! {
                            () = tokio::time::sleep(wait) => {}
                            () = wake.notified() => r.report_soon(),
                        }
                    }
                    Err(why) => {
                        // Not polling, so no longer the runner as far as this computer can say.
                        runner.store(false, std::sync::atomic::Ordering::SeqCst);
                        if said != why {
                            tracing::info!("{id}: rollout: not running: {why}");
                            said = why.to_string();
                        }
                        tokio::time::sleep(super::WAIT).await;
                    }
                }
            }
        });
    }
}
