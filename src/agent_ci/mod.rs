//! docs/specs/agent-ci.md: running CI on this computer. This is the loop: its own tokio task beside the
//! orchestrator loop, with `ciSlots` permits that lanes, sessions and `concurrency` never take or wait for.
//! Each poll it checks Docker, then for each project looks for the project's CI files on its base branch and
//! lists the pull requests to check. Checks are listed here, not run.
pub mod container;
pub mod discovery;
pub mod docker;
pub mod host;
pub mod priority;
pub mod prs;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::config::{self, ConfigFile, ProjectConfig};
use crate::env::Env;
use crate::item_lifecycle::ci_when_ready_active;
use crate::orchestrator::live::LiveHandle;
pub use prs::CheckKey;

/// What the loop reports to the cloud tick and the Now tab's CI header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiLive {
    pub slots: u32,
    /// Permits held now.
    pub running: u32,
    /// `ok`, `missing`, `stopped`, or `off` when `ciSlots` is 0.
    pub docker: &'static str,
    pub docker_reason: Option<String>,
    /// Each project without agent CI, and why.
    pub reasons: BTreeMap<String, String>,
    pub last_result: Option<Value>,
}

impl CiLive {
    /// The tick's `ci: { slots, running, docker, lastResult? }`.
    pub fn tick_json(&self) -> Value {
        let mut ci = json!({ "slots": self.slots, "running": self.running, "docker": self.docker });
        if let Some(last) = &self.last_result {
            ci["lastResult"] = last.clone();
        }
        ci
    }
}

/// The programs the loop starts; a test points them at stubs.
#[derive(Debug, Clone)]
pub struct Programs {
    pub git: OsString,
    pub gh: OsString,
    pub docker: OsString,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            git: "git".into(),
            gh: "gh".into(),
            docker: "docker".into(),
        }
    }
}

pub struct CiLoop {
    programs: Programs,
    live: LiveHandle,
    in_image: bool,
    slot: Arc<Semaphore>,
    permits: u32,
    /// The reason last written to `agent.log` for each project, so a standing reason is said once.
    logged: HashMap<String, String>,
}

impl CiLoop {
    pub fn new(programs: Programs, live: LiveHandle, in_image: bool) -> Self {
        Self {
            programs,
            live,
            in_image,
            slot: Arc::new(Semaphore::new(0)),
            permits: 0,
            logged: HashMap::new(),
        }
    }

    /// The slot: a check holds a permit from its claim to its last status.
    pub fn slot(&self) -> Arc<Semaphore> {
        self.slot.clone()
    }

    fn resize(&mut self, want: u32) {
        if want > self.permits {
            self.slot.add_permits((want - self.permits) as usize);
        } else {
            self.slot.forget_permits((self.permits - want) as usize);
        }
        self.permits = want;
    }

    /// One poll: Docker (probed again every time, so a started Docker resumes without a restart), the slot's
    /// size, and for each project its discovery and the checks to run.
    pub async fn poll(&mut self, cfg: &ConfigFile, env: &Env) -> BTreeMap<String, Vec<CheckKey>> {
        let slots = config::ci_slots(cfg, env);
        let (docker, reason) = if slots == 0 {
            ("off", Some("CI slots is 0: this computer runs no CI".to_string()))
        } else {
            let state = docker::probe(&self.programs.docker, self.in_image).await;
            (state.as_str(), state.reason().map(str::to_string))
        };
        self.resize(if docker == "ok" { slots } else { 0 });
        let mut keys = BTreeMap::new();
        let mut reasons = BTreeMap::new();
        for p in cfg.projects.iter().filter(|p| !p.github_repo.is_empty()) {
            let id = p.id_or_derived();
            let found = if docker == "ok" {
                self.project(&id, p).await
            } else {
                Ok(Vec::new())
            };
            match found {
                Ok(k) => {
                    self.logged.remove(&id);
                    keys.insert(id, k);
                }
                Err(why) => {
                    self.say_once(&id, &why);
                    reasons.insert(id, why);
                }
            }
        }
        let running = self.permits - self.slot.available_permits() as u32;
        self.live.update_ci(|ci| {
            ci.slots = slots;
            ci.running = running;
            ci.docker = docker;
            ci.docker_reason = reason;
            ci.reasons = reasons;
        });
        keys
    }

    /// Says a project's reason in `agent.log` unless it was the last thing said about it. Whether it did.
    fn say_once(&mut self, id: &str, why: &str) -> bool {
        if self.logged.get(id).is_some_and(|w| w == why) {
            return false;
        }
        tracing::info!("{id}: {why}");
        self.logged.insert(id.to_string(), why.to_string());
        true
    }

    /// One project's checks, or why it has none. A project whose backlog can't be reached runs no CI (fail closed).
    async fn project(&self, id: &str, p: &ProjectConfig) -> Result<Vec<CheckKey>, String> {
        let reached = self
            .live
            .snapshot()
            .projects
            .get(id)
            .is_some_and(|l| l.last_tick.is_some() && l.store_error.is_none());
        if !reached {
            return Err("The shared backlog can't be reached: no CI".into());
        }
        let (git, repo, base) = (&*self.programs.git, Path::new(&p.repo_path), p.base_branch.as_str());
        discovery::git(git, repo, &["fetch", "origin", base]).await?;
        discovery::discover(git, repo, base).await?;
        let main = discovery::git(git, repo, &["rev-parse", &format!("origin/{base}")]).await?;
        let list = prs::list(&self.programs.gh, repo, &p.github_repo, base).await?;
        prs::ready(&list, main.trim(), ci_when_ready_active(p))
    }
}

/// The loop, until the agent stops: each `pollIntervalSec`, on the config as saved now.
pub async fn run(mut ci: CiLoop, dir: std::path::PathBuf, env: Env) {
    loop {
        let wait = match config::load(&dir) {
            Ok(cfg) => {
                ci.poll(&cfg, &env).await;
                config::poll_interval_sec(&cfg, &env)
            }
            Err(e) => {
                tracing::warn!("agent CI: config unreadable: {e:#}");
                60
            }
        };
        tokio::time::sleep(Duration::from_secs(wait.into())).await;
    }
}

#[cfg(test)]
mod tests;
