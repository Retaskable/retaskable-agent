//! docs/specs/agent-ci.md: running CI on this computer. This is the loop: its own tokio task beside the
//! orchestrator loop, with `ciSlots` permits that lanes, sessions and `concurrency` never take or wait for.
//! Each poll it checks Docker, then for each project looks for the project's CI files on its base branch and
//! lists the pull requests to check. Checks are listed here, not run. `host.rs` is the fetch step; `report.rs`
//! is the code host side: statuses and the failure comment.
pub mod check;
pub mod claim;
pub mod container;
pub mod discovery;
pub mod docker;
pub mod host;
pub mod priority;
pub mod prs;
pub mod report;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::config::{self, ConfigFile, ProjectConfig};
use crate::env::Env;
use crate::item_lifecycle::ci_when_ready_active;
use crate::orchestrator::live::LiveHandle;
pub use prs::CheckKey;

/// Milliseconds since 1970.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
use tokio::task::JoinHandle;

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
    /// The checks running now, for the Now tab.
    pub checks: Vec<RunningCheck>,
}

/// One running check: a pull request at a short sha, and when it began.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningCheck {
    pub project: String,
    pub pr: u64,
    pub sha: String,
    pub started: Instant,
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
    /// Permits a shrink couldn't take back because checks held them: each is forgotten as it returns.
    owed: Arc<AtomicU32>,
    /// The reason last written to `agent.log` for each project, so a standing reason is said once.
    logged: HashMap<String, String>,
    /// Checks started (running or finished) by this loop, so none runs twice.
    seen: Arc<Mutex<HashSet<(String, CheckKey)>>>,
    /// Milliseconds since 1970; a test injects its own.
    pub clock: Clock,
    /// How often a running check's claim is refreshed.
    pub refresh: Duration,
}

impl CiLoop {
    pub fn new(programs: Programs, live: LiveHandle, in_image: bool) -> Self {
        Self {
            programs,
            live,
            in_image,
            slot: Arc::new(Semaphore::new(0)),
            permits: 0,
            owed: Arc::new(AtomicU32::new(0)),
            logged: HashMap::new(),
            seen: Arc::default(),
            clock: Arc::new(crate::telemetry::now_ms),
            refresh: claim::REFRESH,
        }
    }

    /// The slot: a check holds a permit from its claim to its last status.
    pub fn slot(&self) -> Arc<Semaphore> {
        self.slot.clone()
    }

    fn resize(&mut self, want: u32) {
        if want > self.permits {
            let mut add = want - self.permits;
            // A shrink still waiting on held permits is undone first.
            let taken = (self.owed)
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |o| Some(o - o.min(add)))
                .map_or(0, |before| before.min(add));
            add -= taken;
            self.slot.add_permits(add as usize);
        } else {
            let diff = self.permits - want;
            let forgotten = self.slot.forget_permits(diff as usize) as u32;
            self.owed.fetch_add(diff - forgotten, Ordering::SeqCst);
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
        // Forget keys no longer listed, unless still running. Only for a project whose list was read this poll:
        // otherwise a finished key would run again when the list comes back.
        let running = self.live.snapshot().ci.map(|c| c.checks).unwrap_or_default();
        self.seen.lock().unwrap().retain(|(id, k)| match keys.get(id) {
            Some(l) if docker == "ok" => {
                l.contains(k)
                    || running
                        .iter()
                        .any(|r| r.project == *id && r.pr == k.pr && k.head.starts_with(&r.sha))
            }
            _ => true,
        });
        self.live.update_ci(|ci| {
            ci.slots = slots;
            ci.running = ci.checks.len() as u32;
            ci.docker = docker;
            ci.docker_reason = reason;
            ci.reasons = reasons;
        });
        keys
    }

    /// Starts each key not already seen that finds a free permit (held until its last status). A key with no
    /// permit waits for the next poll.
    pub async fn start(
        &mut self,
        cfg: &ConfigFile,
        env: &Env,
        data_dir: &Path,
        keys: BTreeMap<String, Vec<CheckKey>>,
    ) -> Vec<JoinHandle<()>> {
        let computer = crate::link::machine_name(env);
        let mut tasks = Vec::new();
        for (id, list) in keys {
            let Some(p) = cfg.projects.iter().find(|p| p.id_or_derived() == id) else {
                continue;
            };
            let (git, repo) = (&*self.programs.git, Path::new(&p.repo_path));
            let Ok(image) = discovery::discover(git, repo, &p.base_branch).await else {
                continue;
            };
            for key in list {
                if self.seen.lock().unwrap().contains(&(id.clone(), key.clone())) {
                    continue;
                }
                let Ok(permit) = self.slot.clone().try_acquire_owned() else {
                    return tasks;
                };
                self.seen.lock().unwrap().insert((id.clone(), key.clone()));
                let seen = self.seen.clone();
                let owed = self.owed.clone();
                let job = check::Job {
                    programs: self.programs.clone(),
                    data_dir: data_dir.to_path_buf(),
                    computer: computer.clone(),
                    live: self.live.clone(),
                    id: id.clone(),
                    project: p.clone(),
                    image: image.clone(),
                    key,
                    clock: self.clock.clone(),
                    refresh: self.refresh,
                };
                tasks.push(tokio::spawn(async move {
                    let forget = (job.id.clone(), job.key.clone());
                    // A loser forgets the key, so the next poll looks again (the winner may go stale).
                    if check::run(job).await == check::Ran::Lost {
                        seen.lock().unwrap().remove(&forget);
                    }
                    // A shrink while this ran: this permit is one it owed, so it doesn't return.
                    let owing = owed.try_update(Ordering::SeqCst, Ordering::SeqCst, |o| o.checked_sub(1));
                    if owing.is_ok() {
                        permit.forget();
                    }
                }));
            }
        }
        tasks
    }

    /// Recovery (agent.md section 7.4): leftover directories and containers from a cut check.
    pub async fn recover(&self, cfg: &ConfigFile, data_dir: &Path) {
        for p in cfg.projects.iter().filter(|p| !p.github_repo.is_empty()) {
            check::recover(&self.programs.docker, data_dir, &p.id_or_derived()).await;
        }
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
        let mut keys = prs::ready(&list, main.trim(), ci_when_ready_active(p))?;
        // The baseline: main's own push run (the key is main's sha alone, so a new sha is a new run).
        keys.push(CheckKey::push(main.trim()));
        Ok(keys)
    }
}

/// The loop, until the agent stops: each `pollIntervalSec`, on the config as saved now.
pub async fn run(mut ci: CiLoop, dir: std::path::PathBuf, env: Env) {
    if let Ok(cfg) = config::load(&dir) {
        ci.recover(&cfg, &dir).await;
    }
    loop {
        let wait = match config::load(&dir) {
            Ok(cfg) => {
                let keys = ci.poll(&cfg, &env).await;
                ci.start(&cfg, &env, &dir, keys).await;
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
