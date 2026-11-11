//! The `merge` run (docs/specs/pieces.md sections 3.2 and 3.4): it takes no lane. The loop starts it with the
//! store's merge turn (`merge_next`, docs/specs/merge-queue-from-store.md), its token the turn's. It brings the
//! change up to date with its base (`merge_update`), or lands it with `gh pr merge`, and ends `merged`, `updated`,
//! `conflict` or `failed`; the loop reports that as `merge_result`. A turn lost before it acts ends the run
//! `interrupted`.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::driver::{RunEnd, RunRequest, RunStarter};
use super::watch_run::{Trees, change_ref};
use crate::backlog::runs::{RunKind, RunOutcome};
use crate::ci_watch::gh_out;
use crate::drafts::land::workflow;
use crate::merge_update::{Behind, Update, behind, conflict_instruction, sync_conflict_instruction, update_from_base};
use crate::process_tree::{ProcessTree, with_hook};
use crate::runs::chore::{ChoreKind, chore_of};

/// How often a PR GitHub hasn't worked out is read again.
pub const ASK_EVERY: Duration = Duration::from_secs(15);

type Outcome = Option<(RunOutcome, Value)>;

pub struct MergeRuns {
    gh: PathBuf,
    git: fn(&Path) -> Command,
    ask_every: Duration,
}

impl MergeRuns {
    pub fn new() -> Arc<Self> {
        Self::with(PathBuf::from("gh"), crate::git_bin::command, ASK_EVERY)
    }

    pub fn with(gh: PathBuf, git: fn(&Path) -> Command, ask_every: Duration) -> Arc<Self> {
        Arc::new(Self { gh, git, ask_every })
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
            ask_every: self.ask_every,
            trees: Trees::default(),
            sync: is_sync(&request),
            request,
        };
        tokio::spawn(async move {
            let token = job.request.token.clone();
            let ends = job.request.ends.clone();
            if let Some((outcome, result)) = job.run().await {
                ends.send(RunEnd { token, outcome, result });
            }
        });
        Ok(())
    }
}

struct Job {
    gh: PathBuf,
    git: fn(&Path) -> Command,
    ask_every: Duration,
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
fn wants_update(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    e.contains("not up to date") || e.contains("out of date") || e.contains("behind")
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
        if !flow.we_merge || (!self.sync && flow.method_flag != "--squash") {
            return failed("This project's workflow isn't one where Retaskable squash-merges into the base.");
        }
        let Some(pr) = self.request.piece.change.as_ref().and_then(change_ref) else {
            return failed("The piece has no change to merge.");
        };
        let Some(branch) = self.request.piece.branch.clone() else {
            return failed("The piece has no branch to merge.");
        };
        let piece = super::driver::piece_id(&self.request.piece).to_string();
        let turn = self.request.token.clone();
        let end = self.land(&turn, &pr, &branch, &piece).await;
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

    async fn land(&self, lock: &str, pr: &str, branch: &str, piece: &str) -> Outcome {
        let config = &self.request.context.config;
        let base = config.base_branch.clone();
        let head = format!("origin/{branch}");
        if self.stopped() {
            return None;
        }
        let seen = {
            let base = base.clone();
            self.git(move |git, repo| behind(git, repo, &base, &head)).await
        };
        if self.stopped() {
            return None;
        }
        if seen == Behind::Behind {
            return self.update(lock, branch, piece).await;
        }
        for _ in 0..5 {
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
                Next::Update => return self.update(lock, branch, piece).await,
                Next::Land => return self.merge(lock, pr, branch, piece).await,
                Next::Unknown => {
                    tokio::time::sleep(self.ask_every).await;
                    if self.stopped() {
                        return None;
                    }
                }
            }
        }
        failed("GitHub couldn't say whether the change can merge.")
    }

    /// Merges the base into the change's branch and pushes it.
    async fn update(&self, lock: &str, branch: &str, piece: &str) -> Outcome {
        if self.stopped() {
            return None;
        }
        if !self.request.context.locks.held(lock) {
            return self.lost();
        }
        let config = &self.request.context.config;
        let (base, dir) = (config.base_branch.clone(), PathBuf::from(&config.worktrees_dir));
        let (branch, piece) = (branch.to_string(), piece.to_string());
        let done = {
            let base = base.clone();
            self.git(move |git, repo| update_from_base(git, repo, &dir, &piece, &branch, &base))
                .await
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

    async fn merge(&self, lock: &str, pr: &str, branch: &str, piece: &str) -> Outcome {
        if self.stopped() {
            return None;
        }
        if !self.request.context.locks.held(lock) {
            return self.lost();
        }
        let method = if self.sync { "--merge" } else { "--squash" };
        let args = ["pr", "merge", pr, method, "--delete-branch"];
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
        } else if wants_update(&err) {
            self.update(lock, branch, piece).await
        } else {
            failed(format!("gh pr merge failed: {}", err.trim()))
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
    ) -> R {
        let (git, repo, handle) = (
            self.git,
            PathBuf::from(&self.request.context.config.repo_path),
            self.handle(),
        );
        tokio::task::spawn_blocking(move || with_hook(Box::new(handle), || f(&git, &repo)))
            .await
            .expect("the git task ended")
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
