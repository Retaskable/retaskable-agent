//! The daily round of ideas (docs/specs/agent/09-web-ui.md section 9.3, "Ideas"; `maybeRun` in Retaskable.com's
//! agent/src/ideas.ts). Each minute [`super::Machine::ideas_rounds`] starts [`run`] for a project not already
//! running one. With the `ideas-session` flag On it asks the store whether the `ideas` job is due; the store
//! decides the day from its last `ideas_round`, so a round cut by a restart simply comes due again. A day this
//! computer already tried isn't tried again (a failed round isn't retried every minute).
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::ideas_round::{self, Ask, Job};
use super::playtest_round::Job as PlaytestJob;
use crate::flags::Flags;
use crate::ideas::gather_live;
use crate::jobs_due::DueAsker;

/// The job's name in the store's `jobs_due`.
const JOB: &str = "ideas";

#[derive(Default)]
pub(super) struct State {
    asker: Option<Arc<DueAsker>>,
    /// The last day a round was started for, on this computer.
    tried: Option<String>,
}

impl State {
    /// Lets the next minute ask the store again at once (tests).
    #[cfg(test)]
    pub(super) fn forget_asker(&mut self) {
        self.asker = None;
    }
}

pub(super) struct Daily {
    pub base: PlaytestJob,
    pub flags: Arc<Flags>,
    pub state: Arc<Mutex<State>>,
}

/// Starts today's round when the flag is On and the store says it's due; does nothing otherwise (fail closed).
pub(super) async fn run(daily: Daily) {
    let Daily { base, flags, state } = daily;
    if !flags.on("ideas-session").await {
        return;
    }
    let Some(repo) = base.repo else {
        return;
    };
    let token = match base.keychain.get(&base.backlog_url) {
        Ok(Some(token)) => token,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("{}: ideas round skipped: reading the backlog link: {e:#}", base.id);
            return;
        }
    };
    let asker = {
        let mut st = state.lock().unwrap();
        st.asker
            .get_or_insert_with(|| {
                let start = Instant::now();
                Arc::new(DueAsker::new(
                    base.client.clone(),
                    repo,
                    Some(token.clone()),
                    JOB,
                    move || start.elapsed().as_millis() as u64,
                ))
            })
            .clone()
    };
    let Some(day) = asker.ask().await else {
        return;
    };
    if state.lock().unwrap().tried.replace(day.clone()).as_deref() == Some(day.as_str()) {
        return;
    }
    let live = match base.client.list(repo, Some(&token)).await {
        Ok(items) => {
            let cwd = Path::new(&base.project.repo_path);
            let repo_name = &base.project.github_repo;
            gather_live(Path::new("gh"), cwd, repo_name, &base.project.base_branch, &items)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("{}: reading what went live failed: {e:#}", base.id);
                    Vec::new()
                })
        }
        Err(e) => {
            tracing::warn!("{}: ideas round skipped: reading the backlog: {e}", base.id);
            return;
        }
    };
    let audiences = base.client.audiences(repo, Some(&token)).await.unwrap_or_else(|e| {
        tracing::warn!("{}: reading the user types failed, using the starter set: {e}", base.id);
        Vec::new()
    });
    let job = Job {
        base,
        planner_dir: None,
        live,
        audiences,
        queue: None,
        max_waits: None,
    };
    let ask = Ask {
        fresh: true,
        quiet: true,
        after: None,
    };
    ideas_round::run(job, ask).await;
}
