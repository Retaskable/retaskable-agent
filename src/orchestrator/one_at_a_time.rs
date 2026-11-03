//! What the plan and look runners share (docs/specs/pieces.md section 3.2): the slot only one of them may hold at a
//! time, how a run's task is started and ended exactly once, and the steps `start` takes before it.
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::driver::{RunEnd, RunRequest};
use super::session::exists;
use crate::backlog::runs::RunOutcome;
use crate::config::{EngineChoiceConfig, ModelsConfig, ProjectConfig};
use crate::engines::resolve::resolve_claude_on_path;
use crate::env::Env;
use crate::lanes;
use crate::process_tree::ProcessTree;

/// What a plan or look runner is built with.
#[derive(Clone)]
pub struct Setup {
    pub data_dir: PathBuf,
    pub projects: Vec<ProjectConfig>,
    pub choice: EngineChoiceConfig,
    pub env: Env,
    pub models: ModelsConfig,
}

/// One slot; clones are the same slot.
#[derive(Debug, Clone, Default)]
pub struct OneAtATime(Arc<AtomicBool>);

impl OneAtATime {
    /// Takes the slot, or `None` when a run holds it. The slot is free again when the guard drops, however the run ends.
    pub fn take(&self) -> Option<Held> {
        (!self.0.swap(true, Ordering::SeqCst)).then(|| Held(self.0.clone()))
    }

    pub fn is_free(&self) -> bool {
        !self.0.load(Ordering::SeqCst)
    }
}

/// A run's hold on the slot.
#[derive(Debug)]
pub struct Held(Arc<AtomicBool>);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The session's process tree, kept so a cut that lands after the loop has let the run go is still seen.
#[derive(Clone, Default)]
pub struct Cut(Arc<Mutex<Option<Arc<ProcessTree>>>>);

impl Cut {
    pub fn is_cut(&self) -> bool {
        self.0.lock().unwrap().as_ref().is_some_and(|t| t.was_cut())
    }

    /// Where the session hands its process tree: registered with the run (so a `stop` or the ceiling cuts it) and kept here.
    pub fn tree_sender(&self, request: &RunRequest) -> tokio::sync::oneshot::Sender<Arc<ProcessTree>> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Arc<ProcessTree>>();
        let (processes, token, seen) = (request.processes.clone(), request.token.clone(), self.clone());
        tokio::spawn(async move {
            if let Ok(tree) = rx.await {
                *seen.0.lock().unwrap() = Some(tree.clone());
                processes.add(&token, tree);
            }
        });
        tx
    }
}

/// Runs `work` as the run's task and sends its end through `request.ends` exactly once, even if `work` panics.
/// `None` from `work` (the loop ended the run), a stop or a cut tree sends nothing. The slot frees when it's done.
pub fn spawn_run<F>(held: Held, request: &RunRequest, cut: Cut, work: F)
where
    F: Future<Output = Option<(RunOutcome, Value)>> + Send + 'static,
{
    let (ends, token, stop) = (request.ends.clone(), request.token.clone(), request.stop.clone());
    tokio::spawn(async move {
        let _held = held;
        let end = match tokio::spawn(work).await {
            Ok(end) => end,
            Err(e) => Some((RunOutcome::Failed(format!("the run's task crashed: {e}")), json!({}))),
        };
        if let Some((outcome, result)) = end.filter(|_| !stop.stopped() && !cut.is_cut()) {
            ends.send(RunEnd { token, outcome, result });
        }
    });
}

/// The project a run is for and the `claude` binary, or why the run goes back. `on_claude` says whether the
/// role this run uses is on Claude, the only engine that runs yet.
pub fn claude_project(
    projects: &[ProjectConfig],
    request: &RunRequest,
    env: &Env,
    on_claude: impl Fn(&ProjectConfig) -> bool,
) -> Result<(ProjectConfig, PathBuf), String> {
    let project = projects
        .iter()
        .find(|p| p.id_or_derived() == request.project)
        .ok_or_else(|| format!("no project {} is configured", request.project))?
        .clone();
    if !on_claude(&project) {
        return Err("only Claude sessions run in the Rust agent yet".into());
    }
    let bin = resolve_claude_on_path(env).map_err(|e| format!("{e:#}"))?;
    if !exists(&bin, env) {
        return Err(format!("the claude binary wasn't found ({})", bin.display()));
    }
    Ok((project, bin))
}

/// A read-only run's folder: the project's base checkout as it is, or its repo if that doesn't exist.
pub fn base_or_repo(project: &ProjectConfig) -> PathBuf {
    let base = lanes::base_checkout_path(&PathBuf::from(&project.worktrees_dir));
    if base.exists() {
        base
    } else {
        PathBuf::from(&project.repo_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_and_the_drop_frees_it() {
        let slot = OneAtATime::default();
        let held = slot.take().unwrap();
        assert!(!slot.is_free() && slot.clone().take().is_none());
        drop(held);
        assert!(slot.is_free() && slot.take().is_some());
    }
}
