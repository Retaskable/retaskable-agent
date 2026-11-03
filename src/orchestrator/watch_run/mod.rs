//! The `watch` run (docs/specs/pieces.md section 3.2): it takes no lane, reads the piece's change's checks every
//! 15 seconds until they settle, and ends `green`, `red` or `failed`. The reading and deciding are `ci_watch`'s.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::driver::{RunEnd, RunRequest, RunStarter};
use crate::backlog::runs::{RunKind, RunOutcome};
use crate::ci_coverage::{ci_coverage, read_workflows};
use crate::ci_watch::{AfterError, Decision, GhErrors, decide, read_checks, red_entry};
use crate::runs::{CEILING_MARGIN, ceiling, winding_down};

/// How often the checks are read.
pub const POLL_EVERY: Duration = Duration::from_secs(15);

pub struct WatchRuns {
    gh: PathBuf,
    poll: Duration,
    /// After a `gh` error; `None` is `ci_watch::GH_RETRY`.
    retry: Option<Duration>,
}

impl WatchRuns {
    pub fn new() -> Arc<Self> {
        Self::with(PathBuf::from("gh"), POLL_EVERY, None)
    }

    pub fn with(gh: PathBuf, poll: Duration, retry: Option<Duration>) -> Arc<Self> {
        Arc::new(Self { gh, poll, retry })
    }
}

impl RunStarter for WatchRuns {
    fn kinds(&self) -> Vec<RunKind> {
        vec![RunKind::Watch]
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let (gh, poll, retry) = (self.gh.clone(), self.poll, self.retry);
        tokio::spawn(async move {
            let token = request.token.clone();
            let (outcome, result) = match watch(&request, gh, poll, retry).await {
                Some(end) => end,
                None => return,
            };
            request.ends.send(RunEnd { token, outcome, result });
        });
        Ok(())
    }
}

/// The change as `gh pr checks` takes it: a number or a URL. `None` when the piece has none.
fn change_ref(change: &Value) -> Option<String> {
    match change {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(o) => ["number", "url"].iter().find_map(|k| change_ref(o.get(*k)?)),
        _ => None,
    }
}

/// How the run ended; `None` when the loop stopped it (it has already ended it `interrupted`).
async fn watch(
    request: &RunRequest,
    gh: PathBuf,
    poll: Duration,
    retry: Option<Duration>,
) -> Option<(RunOutcome, Value)> {
    let failed = |why: &str| Some((RunOutcome::Failed(why.into()), json!({})));
    let Some(pr) = request.piece.change.as_ref().and_then(change_ref) else {
        return failed("The piece has no change to watch.");
    };
    let config = &request.context.config;
    let repo = PathBuf::from(&config.repo_path);
    let coverage = {
        let (repo, base) = (repo.clone(), format!("origin/{}", config.base_branch));
        tokio::task::spawn_blocking(move || ci_coverage(&read_workflows(&repo, &base), &base).answer)
            .await
            .unwrap_or_default()
    };
    let start = Instant::now();
    let limit = ceiling(RunKind::Watch).saturating_sub(CEILING_MARGIN);
    let mut errors = GhErrors::default();
    loop {
        if request.handle.stopped() {
            return None;
        }
        if winding_down() {
            return Some((RunOutcome::Interrupted, json!({})));
        }
        if start.elapsed() >= limit {
            return failed("The watch reached its time limit before CI settled.");
        }
        let read = {
            let (gh, repo, pr, handle) = (gh.clone(), repo.clone(), pr.clone(), request.handle.clone());
            tokio::task::spawn_blocking(move || read_checks(&gh, &repo, &pr, &|t| handle.add(t))).await
        };
        if request.handle.stopped() {
            return None;
        }
        let checks = match read {
            Ok(Ok(checks)) => checks,
            Ok(Err(e)) => match errors.error() {
                AfterError::Retry(d) => {
                    tokio::time::sleep(retry.unwrap_or(d)).await;
                    continue;
                }
                AfterError::Failed => return failed(&format!("The code host kept failing: {e:#}")),
            },
            Err(e) => return failed(&format!("Reading the checks broke: {e}")),
        };
        errors.answered();
        match decide(
            &checks,
            coverage,
            start.elapsed(),
            config.tuning.ci_grace_minutes,
            config.tuning.ci_max_minutes,
        ) {
            Decision::Wait => tokio::time::sleep(poll).await,
            Decision::Green { note } => return Some((RunOutcome::Green, json!({ "note": note }))),
            Decision::Failed { why } => return failed(&why),
            Decision::Red { failing } => {
                let (gh, repo, handle) = (gh.clone(), repo.clone(), request.handle.clone());
                let entries = tokio::task::spawn_blocking(move || {
                    failing
                        .iter()
                        .map(|f| red_entry(&gh, &repo, f, &|t| handle.add(t)))
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default();
                if request.handle.stopped() {
                    return None;
                }
                let checks: Vec<Value> = entries
                    .iter()
                    .map(|e| json!({ "name": e.name, "link": e.link, "log": e.log }))
                    .collect();
                return Some((RunOutcome::Red, json!({ "checks": checks })));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
