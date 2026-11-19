//! The `watch` run (docs/specs/pieces.md section 3.2): it takes no lane, reads the piece's change's checks every
//! 45 seconds until they settle (the TypeScript agent's `watchSeconds`, and [`MIN_GAP`] at the least between two reads),
//! and ends `green`, `red` or `failed`. The store's `work_available` push (it follows every `ci_finish`) reads them at
//! once: the poll is only the fallback for a push that never came. The reading and deciding are `ci_watch`'s.
use crate::text::is_blank;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::driver::{RunRequest, RunStarter, piece_id};
use super::run_guard::spawn_ended;
use crate::backlog::runs::{RunKind, RunOutcome, RunPiece};
use crate::ci_coverage::{covering, has_ci_file, read_workflows};
use crate::ci_watch::{AfterError, Check, CheckState, Decision, GhErrors, decide, read_checks, red_entry};
use crate::process_tree::ProcessTree;
use crate::runs::winding_down;
use crate::telemetry::{Event, event};
use crate::work_signal::CiListener;

/// How often the checks are read when no push says CI moved: the TypeScript agent's `watchSeconds` (45), which keeps a
/// computer's watches inside GitHub's hourly limit (ci-watch-through-store.md section 3.6).
pub const POLL_EVERY: Duration = Duration::from_secs(45);
/// The least time between two reads, however many pushes come.
pub const MIN_GAP: Duration = Duration::from_secs(5);

/// The run's latest `gh` tree, kept so a stop that lands between polls is still seen after the loop has let the
/// run go. Earlier trees are the loop's alone, so it can drop them once they finish.
#[derive(Clone, Default)]
pub(super) struct Trees(Arc<Mutex<Option<Arc<ProcessTree>>>>);

impl Trees {
    pub(super) fn add(&self, request: &RunRequest, tree: Arc<ProcessTree>) {
        // The previous one is let go before the loop hears of this one, so the loop's prune sees it as finished.
        *self.0.lock().unwrap() = Some(tree.clone());
        request.processes.add(&request.token, tree);
    }

    pub(super) fn stopped(&self) -> bool {
        self.0.lock().unwrap().as_ref().is_some_and(|t| t.was_cut())
    }
}

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
        let (token, ends) = (request.token.clone(), request.ends.clone());
        spawn_ended(
            ends,
            token,
            "watch",
            async move { watch(&request, gh, poll, retry).await },
        );
        Ok(())
    }
}

/// The change as `gh pr checks` takes it: a number or a URL. `None` when the piece has none.
pub(super) fn change_ref(change: &Value) -> Option<String> {
    match change {
        Value::String(s) if !is_blank(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(o) => ["number", "url"].iter().find_map(|k| change_ref(o.get(*k)?)),
        _ => None,
    }
}

/// The number of the open change on the piece's branch (its own `branch`, else the name its build gives it), as
/// `gh pr list --head <branch> --state open` finds it.
async fn open_change_on_branch(piece: &RunPiece, gh: &Path, repo: &Path) -> anyhow::Result<Option<String>> {
    let branch = match piece.branch.as_deref().filter(|b| !is_blank(b)) {
        Some(b) => b.trim().to_string(),
        None => super::session::piece_branch_name(piece),
    };
    let (gh, repo) = (gh.to_path_buf(), repo.to_path_buf());
    let found =
        tokio::task::spawn_blocking(move || crate::implementer::publish::find_open(&gh, &repo, &branch)).await??;
    Ok(found.map(|p| p.number.to_string()))
}

/// The CI state a read shows, as the `ci` event names it. A read that ends Green with the no-CI note is `none`.
fn ci_state(checks: &[Check], decision: &Decision) -> &'static str {
    let live = || checks.iter().filter(|c| c.state != CheckState::Skipping);
    if matches!(decision, Decision::Green { note: Some(_) }) || checks.is_empty() {
        "none"
    } else if live().any(|c| matches!(c.state, CheckState::Fail | CheckState::Cancel)) {
        "fail"
    } else if live().any(|c| c.state == CheckState::Pending) {
        "pending"
    } else {
        "pass"
    }
}

/// Waits for the next read: `poll`, or less when the store's push says CI moved (never within [`MIN_GAP`] of the last read).
async fn pause(pushes: &mut Option<CiListener>, poll: Duration, read_started: Instant) {
    let pushed = async {
        match pushes {
            Some(p) => p.changed().await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        () = tokio::time::sleep(poll) => {}
        () = pushed => tokio::time::sleep(MIN_GAP.saturating_sub(read_started.elapsed())).await,
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
    let config = &request.context.config;
    let repo = PathBuf::from(&config.repo_path);
    let pr = match request.piece.recorded_change().as_ref().and_then(change_ref) {
        Some(pr) => pr,
        // A run that died after opening the change leaves the store without it: the change is on the host, under the
        // piece's branch. Watch it; the store gets it with the next `pr_opened`.
        None => match open_change_on_branch(&request.piece, &gh, &repo).await {
            Ok(Some(number)) => number,
            Ok(None) => return failed("The piece has no change to watch, and no open one is on its branch."),
            Err(e) => {
                return failed(&format!(
                    "The piece has no change to watch, and looking for one failed: {e:#}"
                ));
            }
        },
    };
    let coverage = {
        let (repo, base) = (repo.clone(), format!("origin/{}", config.base_branch));
        tokio::task::spawn_blocking(move || {
            covering(&read_workflows(&repo, &base), &base, has_ci_file(&repo, &base)).answer
        })
        .await
        .unwrap_or_default()
    };
    let trees = Trees::default();
    let start = Instant::now();
    // How long the store lets the watch run (`run_take`'s `ceilingMs`); it ends it itself with a `stop` on a beat, which is what ends it.
    let limit = request.ceiling();
    let mut errors = GhErrors::default();
    let mut last_state = "";
    let mut pushes = request.context.wakes.as_ref().map(|w| w.ci_listener(&request.project));
    loop {
        if trees.stopped() {
            return None;
        }
        if winding_down() {
            return Some((RunOutcome::Interrupted, json!({})));
        }
        if limit.is_some_and(|l| start.elapsed() >= l) {
            return failed("The watch reached its time limit before CI settled.");
        }
        let read_started = Instant::now();
        let read = {
            let (gh, repo, pr, handle) = (gh.clone(), repo.clone(), pr.clone(), (request.clone(), trees.clone()));
            tokio::task::spawn_blocking(move || read_checks(&gh, &repo, &pr, &|t| handle.1.add(&handle.0, t))).await
        };
        if trees.stopped() {
            return None;
        }
        let checks = match read {
            Ok(Ok(checks)) => checks,
            Ok(Err(e)) => match errors.error(config.tuning.store.map_or(0, |t| t.gh_errors_max)) {
                AfterError::Retry(d) => {
                    tokio::time::sleep(retry.unwrap_or(d)).await;
                    continue;
                }
                AfterError::Failed => return failed(&format!("The code host kept failing: {e:#}")),
            },
            Err(e) => return failed(&format!("Reading the checks broke: {e}")),
        };
        errors.answered();
        // The waits are the store's numbers; unknown, nothing is decided and the poll goes on.
        let Some(t) = config.tuning.store else {
            pause(&mut pushes, poll, read_started).await;
            continue;
        };
        let decision = decide(&checks, coverage, start.elapsed(), t.ci_grace_minutes, t.ci_max_minutes);
        // The loop writes it to the log only with `ci-merged-events` On; never the same state twice in a row.
        let state = ci_state(&checks, &decision);
        if state != std::mem::replace(&mut last_state, state) {
            request.events.send(Event::Ci(event::Ci {
                item: Some(piece_id(&request.piece).into()),
                state: Some(state.into()),
                ms: Some(start.elapsed().as_millis() as f64),
                ..Default::default()
            }));
        }
        match decision {
            Decision::Wait => pause(&mut pushes, poll, read_started).await,
            Decision::Green { note } => return Some((RunOutcome::Green, json!({ "note": note }))),
            Decision::Failed { why } => return failed(&why),
            Decision::Red { failing } => {
                let (gh, repo, handle) = (gh.clone(), repo.clone(), (request.clone(), trees.clone()));
                let entries = tokio::task::spawn_blocking(move || {
                    failing
                        .iter()
                        .map(|f| red_entry(&gh, &repo, f, &|t| handle.1.add(&handle.0, t)))
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default();
                if trees.stopped() {
                    return None;
                }
                let checks: Vec<Value> = entries
                    .iter()
                    .map(|e| json!({ "name": e.name, "link": e.link, "log": e.log }))
                    .collect();
                // `what` is the store's: the fix run's instruction (`str(result, "what", 4000)` in pieceDone).
                let failed: Vec<crate::ci_checks::FailedCheck> = entries
                    .iter()
                    .map(|e| crate::ci_checks::FailedCheck {
                        name: e.name.clone(),
                        link: e.link.clone(),
                        cancelled: false,
                        completed_at: None,
                    })
                    .collect();
                let logs: Vec<Option<String>> = entries.iter().map(|e| Some(e.log.clone())).collect();
                let what =
                    crate::ci_checks::failed_note(&failed, &logs, "pull request", Some(crate::ci_checks::WHAT_MAX));
                return Some((RunOutcome::Red, json!({ "what": what, "checks": checks })));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
