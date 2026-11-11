//! The daily deprecated-specs check (docs/specs/agent/06.7-chores-never-idle.md "Deprecated specs";
//! `maybeCheck`/`check` in Retaskable.com's agent/src/deprecations.ts). Each minute
//! [`super::Machine::deprecation_checks`] starts [`run`] for a project not already running one. It asks the store for the project's day, and once a day reads the top-level
//! specs on `origin/<base>`; each deprecated spec past its `Remove after` day with no `remove-<name>` item gets one,
//! added under the store's `deprecation-<name>` lock. Paused or not; no lane and no session.
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::playtest_round::Job;
use super::report::spec_files;
use crate::backlog::runs::LockTake;
use crate::deprecations::{read_deprecations, removal_due, removal_id, removal_item};
use crate::jobs_due::DayAsker;

/// A failed check is tried again after this long.
const RETRY_MS: u64 = 10 * 60_000;

#[derive(Default)]
pub(super) struct State {
    asker: Option<Arc<DayAsker>>,
    /// The last day a check completed for.
    checked: Option<String>,
    retry_at: Option<Instant>,
}

impl State {
    /// Lets the next minute ask the store again at once (tests).
    #[cfg(test)]
    pub(super) fn forget_asker(&mut self) {
        self.asker = None;
    }

    /// Lets a failed check be retried at once (tests).
    #[cfg(test)]
    pub(super) fn forget_retry(&mut self) {
        self.retry_at = None;
    }
}

pub(super) struct Daily {
    pub base: Job,
    pub state: Arc<Mutex<State>>,
}

/// Checks today's deprecations when the store gives the day; does nothing otherwise.
pub(super) async fn run(daily: Daily) {
    let Daily { base, state } = daily;
    let Some(repo) = base.repo else {
        return;
    };
    let token = match base.keychain.get(&base.backlog_url) {
        Ok(Some(token)) => token,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("{}: deprecated specs skipped: reading the backlog link: {e:#}", base.id);
            return;
        }
    };
    let asker = {
        let mut st = state.lock().unwrap();
        if st.retry_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        st.asker
            .get_or_insert_with(|| {
                let start = Instant::now();
                Arc::new(DayAsker::new(
                    base.client.clone(),
                    repo,
                    Some(token.clone()),
                    move || start.elapsed().as_millis() as u64,
                ))
            })
            .clone()
    };
    let Some(day) = asker.ask().await else {
        return;
    };
    if state.lock().unwrap().checked.as_deref() == Some(day.as_str()) {
        return;
    }
    let result = check(&base, repo, &token, &day).await;
    let mut st = state.lock().unwrap();
    match result {
        Ok(()) => {
            st.checked = Some(day);
            st.retry_at = None;
        }
        Err(e) => {
            tracing::warn!(
                "{}: deprecated specs check failed, trying again in 10 minutes: {e}",
                base.id
            );
            st.retry_at = Some(Instant::now() + Duration::from_millis(RETRY_MS));
        }
    }
}

async fn check(base: &Job, repo: u64, token: &str, day: &str) -> Result<(), String> {
    let at = format!("origin/{}", base.project.base_branch);
    let files = spec_files(Path::new(&base.project.repo_path), &at)
        .await
        .ok_or_else(|| format!("couldn't read the specs on {at}"))?;
    let (deprecations, problems) = read_deprecations(&files);
    for p in &problems {
        tracing::warn!("{}: {}'s `{}` isn't usable: {}", base.id, p.spec, p.line, p.message);
    }
    if deprecations.is_empty() {
        return Ok(());
    }
    let existing: HashSet<String> = base
        .client
        .list(repo, Some(token))
        .await
        .map_err(|e| format!("reading the backlog: {e}"))?
        .into_iter()
        .map(|i| i.id)
        .collect();
    let mut failed = None;
    for d in deprecations.iter().filter(|d| removal_due(d, day, &existing)) {
        if let Err(e) = add_removal(base, repo, token, d).await {
            tracing::warn!("{}: adding {} failed: {e}", base.id, removal_id(d));
            failed = Some(e);
        } else {
            tracing::info!(
                "{}: {}'s removal date {} has passed: added {}",
                base.id,
                d.spec,
                d.remove_after,
                removal_id(d)
            );
        }
    }
    failed.map_or(Ok(()), Err)
}

/// Adds one removal item under its `deprecation-<name>` lock; another holder skips it. A `conflict` counts as added.
async fn add_removal(base: &Job, repo: u64, token: &str, d: &crate::deprecations::Deprecation) -> Result<(), String> {
    let name = format!("deprecation-{}", d.name);
    let lock = match &base.locks {
        Some(locks) => {
            let note = format!("deprecation on {}", base.machine);
            match base.client.lock_take(repo, Some(token), &name, Some(&note)).await {
                Ok(LockTake::Held(lock)) => {
                    locks.add(&lock, &name, Instant::now());
                    Some(lock)
                }
                Ok(LockTake::HeldBy { .. }) => return Ok(()),
                Err(e) => return Err(format!("taking {name}: {e}")),
            }
        }
        None => match base.client.lock(repo, Some(token), &name, true).await {
            Ok(a) if a.by.as_deref() == Some(base.machine.as_str()) => None,
            Ok(_) => return Ok(()),
            Err(e) => return Err(format!("taking {name}: {e}")),
        },
    };
    let added = base.client.add_removal(repo, Some(token), removal_item(d)).await;
    let dropped = match (&base.locks, &lock) {
        (Some(locks), Some(lock)) => {
            let r = base.client.lock_drop(repo, Some(token), lock).await;
            locks.remove(lock);
            r
        }
        _ => base.client.lock(repo, Some(token), &name, false).await.map(|_| ()),
    };
    if let Err(e) = dropped {
        tracing::warn!("{}: dropping {name} failed: {e}", base.id);
    }
    match added {
        Err(e) if e.code() != Some("conflict") => Err(e.to_string()),
        _ => Ok(()),
    }
}
