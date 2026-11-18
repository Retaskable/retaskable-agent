//! The `merge` run (docs/specs/pieces.md sections 3.2 and 3.4): it takes no lane. The loop starts it with the
//! store's merge turn (`merge_next`, docs/specs/merge-queue-from-store.md), its token the turn's. It brings the
//! change up to date with its base (`merge_update`), or lands it with `gh pr merge`, and ends `merged`, `updated`,
//! `conflict` or `failed`; the loop reports that as `merge_result`. A turn lost before it acts ends the run
//! `interrupted`. Where the project's `land` is `auto_merge` (or `queue`) it hands the change to GitHub and waits for it
//! holding the turn (`auto.rs`).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::driver::{RunRequest, RunStarter};
use super::run_guard::spawn_ended;
use super::watch_run::{Trees, change_ref};

use crate::backlog::runs::{BeatVerdict, RunKind, RunOutcome};
use crate::ci_watch::gh_out;
use crate::drafts::land::workflow;
use crate::merge_update::{Behind, Update, behind, conflict_instruction, sync_conflict_instruction, update_from_base};
use crate::process_tree::{ProcessTree, with_hook};
use crate::runs::chore::{ChoreKind, chore_of};
use crate::runs::{MERGING, UPDATING};

mod auto;

/// How often a PR GitHub hasn't worked out is read again.
pub const ASK_EVERY: Duration = Duration::from_secs(15);
/// Conflict rounds a run has used when it starts: a piece's merge run ends with its first update (the piece goes
/// to CI and comes back as a new run), as in the TypeScript agent, where each such run counts from none.
const ROUNDS: u32 = 0;

/// The waits of a merge run. `unknown_for` (how long a PR may stay unworked-out before the branch is brought up to date to make
/// GitHub look again, github-workflow.md section 4.4) and `retry_after` (how long a merge GitHub wasn't ready for waits) are the
/// store's `mergeUnknownMinutes` and `mergeRetrySeconds` unless set here, which only the tests do to shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Delays {
    pub ask_every: Duration,
    pub unknown_for: Option<Duration>,
    pub retry_after: Option<Duration>,
}

impl Default for Delays {
    fn default() -> Self {
        Self {
            ask_every: ASK_EVERY,
            unknown_for: None,
            retry_after: None,
        }
    }
}

type Outcome = Option<(RunOutcome, Value)>;

pub struct MergeRuns {
    gh: PathBuf,
    git: fn(&Path) -> Command,
    delays: Delays,
}

impl MergeRuns {
    pub fn new() -> Arc<Self> {
        Self::with(PathBuf::from("gh"), crate::git_bin::command, Delays::default())
    }

    pub fn with(gh: PathBuf, git: fn(&Path) -> Command, delays: Delays) -> Arc<Self> {
        Arc::new(Self { gh, git, delays })
    }
}

impl RunStarter for MergeRuns {
    fn kinds(&self) -> Vec<RunKind> {
        vec![RunKind::Merge]
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        let job = Job {
            gh: self.gh.clone(),
            git: self.git,
            delays: self.delays,
            trees: Trees::default(),
            sync: is_sync(&request),
            request,
        };
        let (token, ends) = (job.request.token.clone(), job.request.ends.clone());
        spawn_ended(ends, token, "merge", async move { job.run().await });
        Ok(())
    }
}

struct Job {
    gh: PathBuf,
    git: fn(&Path) -> Command,
    delays: Delays,
    trees: Trees,
    request: RunRequest,
    /// A main sync (`sync_main` chore): merged with a merge commit, never rebased.
    sync: bool,
}

fn is_sync(request: &RunRequest) -> bool {
    chore_of(&request.piece).is_some_and(|c| c.kind == ChoreKind::SyncMain)
}

fn failed(why: impl Into<String>) -> Outcome {
    Some((RunOutcome::Failed(why.into()), json!({})))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct View {
    mergeable: String,
    merge_state_status: String,
    state: String,
    /// Set while GitHub's auto-merge is on for the change.
    auto_merge_request: Option<Value>,
}

enum Next {
    Update,
    Land,
    Merged,
    Closed,
    Unknown,
}

impl View {
    fn next(&self) -> Next {
        match self.state.as_str() {
            "MERGED" => return Next::Merged,
            "CLOSED" => return Next::Closed,
            _ => {}
        }
        if self.mergeable == "CONFLICTING" || matches!(self.merge_state_status.as_str(), "DIRTY" | "BEHIND") {
            Next::Update
        } else if self.mergeable == "MERGEABLE" {
            Next::Land
        } else {
            Next::Unknown
        }
    }
}

/// A `gh pr merge` refusal that says the branch rules want the change brought up to date.
pub(in crate::orchestrator) fn wants_update(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    e.contains("not up to date") || e.contains("out of date") || e.contains("behind")
}

/// A `gh pr merge` refusal that says GitHub isn't ready to merge yet (retried on its own count), not that it
/// didn't answer.
pub(in crate::orchestrator) fn not_ready(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    [
        "base branch was modified",
        "not mergeable",
        "mergeable state",
        "still being",
        "try the merge again",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// A `gh pr merge --merge` refusal because the repo doesn't allow merge commits.
fn refuses_merge_commits(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    e.contains("merge commit") && (e.contains("not allowed") || e.contains("disabled") || e.contains("not enabled"))
}

impl Job {
    fn conflict_text(&self, base: &str, files: &[String]) -> String {
        if self.sync {
            sync_conflict_instruction(base, files)
        } else {
            conflict_instruction(base, files)
        }
    }

    /// How the run ended; `None` when a stop cut one of its processes (the loop has already ended it).
    async fn run(&self) -> Outcome {
        let ctx = &self.request.context;
        let flow = workflow(&ctx.config);
        // A piece on a project where a person merges runs on their word, so it lands like `retaskable` (pieces.md 3.6).
        // A main sync is Retaskable's own merge and never a person's.
        let lands = flow.we_merge || (flow.land == "person" && !self.sync) || self.auto();
        if !lands {
            return failed("This project's workflow has an unknown way to land a change.");
        }
        let Some(pr) = self.request.piece.recorded_change().as_ref().and_then(change_ref) else {
            return failed("The piece has no change to merge.");
        };
        let Some(branch) = self.request.piece.branch.clone() else {
            return failed("The piece has no branch to merge.");
        };
        let piece = super::driver::piece_id(&self.request.piece).to_string();
        let turn = self.request.token.clone();
        let end = self.land(&turn, &pr, &branch, &piece, 0).await;
        ctx.locks.remove(&turn);
        end
    }

    /// The loop has ended the run: a beat's `stop` or the ceiling, seen whether or not a process was running.
    fn stopped(&self) -> bool {
        self.request.stop.stopped() || self.trees.stopped()
    }

    fn lost(&self) -> Outcome {
        Some((RunOutcome::Interrupted, json!({})))
    }

    async fn land(&self, lock: &str, pr: &str, branch: &str, piece: &str, tries: u32) -> Outcome {
        let config = &self.request.context.config;
        let base = config.base_branch.clone();
        let head = format!("origin/{branch}");
        if self.stopped() {
            return None;
        }
        // A restart finds auto-merge already on, or the change already merged: it waits, it doesn't push or enable again.
        if tries == 0
            && self.auto()
            && let Err(end) = self.reopened(lock, pr, branch, piece).await
        {
            return end;
        }
        let seen = {
            let base = base.clone();
            self.git(move |git, repo| behind(git, repo, &base, &head))
                .await
                .unwrap_or(Behind::CantTell)
        };
        if self.stopped() {
            return None;
        }
        // Once the conflict rounds are used up, a change that is only behind merges as it is.
        if seen == Behind::Behind && self.may_update() {
            return self.update(lock, branch, piece).await;
        }
        let asked = tokio::time::Instant::now();
        loop {
            let view = match self
                .gh_json::<View>(&["pr", "view", pr, "--json", "mergeable,mergeStateStatus,state"])
                .await
            {
                Ok(v) => v,
                Err(why) => return failed(why),
            };
            if self.stopped() {
                return None;
            }
            match view.next() {
                Next::Merged => return self.merged(pr).await,
                Next::Closed => return failed("The change was closed without merging."),
                Next::Update if !self.may_update() => return self.out_of_rounds(),
                Next::Update => return self.update(lock, branch, piece).await,
                Next::Land if self.auto() => return self.hand_off(lock, pr, branch, piece).await,
                Next::Land => return self.merge(lock, pr, branch, piece, tries).await,
                Next::Unknown if asked.elapsed() >= self.unknown_for() => {
                    return self.unknown_too_long(lock, branch, piece).await;
                }
                Next::Unknown => {
                    tokio::time::sleep(self.delays.ask_every).await;
                    if self.stopped() {
                        return None;
                    }
                }
            }
        }
    }

    /// A conflict with no `maxConflictRounds` left to bring the change up to date.
    fn out_of_rounds(&self) -> Outcome {
        failed(format!(
            "Still conflicting with {} after {ROUNDS} tries to bring it up to date (maxConflictRounds {}): other changes keep merging into the same lines first.",
            self.request.context.config.base_branch,
            self.cap()
        ))
    }

    /// The store's `mergeUnknownMinutes`; unknown, never (the branch is not updated: nothing is guessed).
    fn unknown_for(&self) -> Duration {
        self.delays.unknown_for.unwrap_or_else(|| {
            self.request
                .context
                .config
                .tuning
                .store
                .map_or(Duration::MAX, |t| t.merge_unknown())
        })
    }

    /// The store's `mergeRetrySeconds`; unknown, no merge is retried (`maxMergeRetries` is unknown too) so it is not waited for.
    pub(super) fn retry_after(&self) -> Duration {
        self.delays.retry_after.unwrap_or_else(|| {
            self.request
                .context
                .config
                .tuning
                .store
                .map_or(Duration::ZERO, |t| t.merge_retry())
        })
    }

    fn cap(&self) -> u32 {
        self.request.context.config.max_conflict_rounds
    }

    /// Whether another update from the base is within `maxConflictRounds`.
    fn may_update(&self) -> bool {
        ROUNDS < self.cap()
    }

    /// GitHub hasn't worked the change out for as long as `unknown_for`: the branch is brought up to date, which makes
    /// it look again; a main sync, or no rounds left, needs a person instead.
    async fn unknown_too_long(&self, lock: &str, branch: &str, piece: &str) -> Outcome {
        if self.sync || !self.may_update() {
            let minutes = self.unknown_for().as_secs() / 60;
            return failed(format!(
                "GitHub has not worked out whether the change merges for over {minutes} minutes, and bringing it up to date {ROUNDS} times did not help."
            ));
        }
        self.update(lock, branch, piece).await
    }

    /// Merges the base into the change's branch and pushes it.
    async fn update(&self, lock: &str, branch: &str, piece: &str) -> Outcome {
        if self.stopped() {
            return None;
        }
        if !self.request.context.locks.held(lock) {
            return self.lost();
        }
        self.request.context.locks.set_phase(lock, UPDATING);
        let config = &self.request.context.config;
        let (base, dir) = (config.base_branch.clone(), PathBuf::from(&config.worktrees_dir));
        let (branch, piece) = (branch.to_string(), piece.to_string());
        let done = {
            let base = base.clone();
            self.git(move |git, repo| update_from_base(git, repo, &dir, &piece, &branch, &base))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!(e)))
        };
        if self.stopped() {
            return None;
        }
        match done {
            Ok(Update::Updated { head }) => Some((RunOutcome::Updated, json!({ "head": head }))),
            Ok(Update::Conflict { files }) => Some((
                RunOutcome::Conflict,
                json!({ "instruction": self.conflict_text(&base, &files), "files": files }),
            )),
            Err(e) => failed(format!("Bringing the change up to date failed: {e:#}")),
        }
    }

    async fn merge(&self, lock: &str, pr: &str, branch: &str, piece: &str, tries: u32) -> Outcome {
        if self.stopped() {
            return None;
        }
        if !self.request.context.locks.held(lock) {
            return self.lost();
        }
        if !self.beat_before_merging(lock).await {
            return self.lost();
        }
        let flow = workflow(&self.request.context.config);
        let method = if self.sync { "--merge" } else { flow.method_flag };
        let mut args = vec!["pr", "merge", pr, method];
        if flow.delete_branch {
            args.push("--delete-branch");
        }
        let (_, err, ok) = match self.gh(&args).await {
            Ok(out) => out,
            Err(why) => return failed(why),
        };
        if self.stopped() {
            return None;
        }
        if ok {
            self.merged(pr).await
        } else if self.sync && refuses_merge_commits(&err) {
            Some((
                RunOutcome::Questions,
                json!({ "questions": [{
                    "question": "Allow merge commits in the repo's settings, or sync by hand",
                    "options": ["Merge commits are allowed now", "I'll sync by hand"],
                }] }),
            ))
        } else if wants_update(&err) && self.may_update() && self.still_behind(branch).await {
            // The branch rules want it up to date, and it is: only then is the base merged in.
            self.update(lock, branch, piece).await
        } else if not_ready(&err)
            && tries
                < self
                    .request
                    .context
                    .config
                    .tuning
                    .store
                    .map_or(0, |t| t.max_merge_retries)
        {
            self.request.context.locks.set_phase(lock, UPDATING);
            tokio::time::sleep(self.retry_after()).await;
            if self.stopped() {
                return None;
            }
            // The turn is kept; the change is read again and the turn beaten again before the next try.
            Box::pin(self.land(lock, pr, branch, piece, tries + 1)).await
        } else {
            failed(format!("gh pr merge failed: {}", err.trim()))
        }
    }

    /// Whether git says the base has commits the change's branch lacks.
    async fn still_behind(&self, branch: &str) -> bool {
        let (base, head) = (
            self.request.context.config.base_branch.clone(),
            format!("origin/{branch}"),
        );
        let seen = (self.git(move |git, repo| behind(git, repo, &base, &head)).await).unwrap_or(Behind::CantTell);
        seen == Behind::Behind
    }

    /// The beat just before the host call (`merge_beat`, phase `merging`): only while the store says the turn is
    /// still ours does anything merge. A store that can't be asked counts as not ours (fail closed); the turn then
    /// goes back unmerged and is asked for again.
    async fn beat_before_merging(&self, lock: &str) -> bool {
        let ctx = &self.request.context;
        ctx.locks.set_phase(lock, MERGING);
        let held = ctx.client.merge_beat(ctx.repo, Some(&ctx.token), lock, MERGING).await;
        if self.stopped() {
            return false;
        }
        match held {
            Ok(true) => true,
            Ok(false) => {
                ctx.locks.apply_beat(&[(lock.to_string(), BeatVerdict::Stop)].into());
                false
            }
            Err(e) => {
                tracing::warn!(turn = %lock, "the beat before merging wasn't taken, so nothing is merged: {e}");
                false
            }
        }
    }

    /// `merged`, with the merge commit read back from gh.
    async fn merged(&self, pr: &str) -> Outcome {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Merge {
            oid: Option<String>,
        }
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "camelCase", default)]
        struct Read {
            merge_commit: Option<Merge>,
        }
        let read = self.gh_json::<Read>(&["pr", "view", pr, "--json", "mergeCommit"]).await;
        if self.stopped() {
            return None;
        }
        let commit = read.ok().and_then(|r| r.merge_commit?.oid);
        Some((RunOutcome::Merged, json!({ "commit": commit })))
    }

    /// Runs `f` on a blocking thread in the project's checkout, each git it starts a tree of the run.
    async fn git<R: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn Fn(&Path) -> Command, &Path) -> R + Send + 'static,
    ) -> Result<R, String> {
        let (git, repo, handle) = (
            self.git,
            PathBuf::from(&self.request.context.config.repo_path),
            self.handle(),
        );
        tokio::task::spawn_blocking(move || with_hook(Box::new(handle), || f(&git, &repo)))
            .await
            .map_err(|e| format!("the git task ended: {e}"))
    }

    fn handle(&self) -> impl Fn(Arc<ProcessTree>) + Send + 'static {
        let (request, trees) = (self.request.clone(), self.trees.clone());
        move |tree| trees.add(&request, tree)
    }

    /// `gh <args>` in the project's checkout: its stdout, stderr and whether it succeeded.
    async fn gh(&self, args: &[&str]) -> Result<(String, String, bool), String> {
        let (gh, repo, handle) = (
            self.gh.clone(),
            PathBuf::from(&self.request.context.config.repo_path),
            self.handle(),
        );
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            gh_out(&gh, &repo, &refs, &handle).map_err(|e| format!("Starting gh failed: {e:#}"))
        })
        .await
        .map_err(|e| format!("Reading from gh broke: {e}"))?
    }

    async fn gh_json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let (out, err, ok) = self.gh(args).await?;
        if !ok {
            return Err(format!("gh {} failed: {}", args[..2].join(" "), err.trim()));
        }
        serde_json::from_str(&out).map_err(|e| format!("Couldn't read gh's answer: {e}"))
    }
}

#[cfg(test)]
mod tests;
